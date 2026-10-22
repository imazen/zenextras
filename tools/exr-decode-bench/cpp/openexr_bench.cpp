// OpenEXR reference side of the decode benchmark. Test tooling only: it links
// the OpenEXR C++ library and is never part of a shipped crate's graph.
//
// Commands:
//   openexr-bench version
//   openexr-bench encode <in.exr> <out.exr> <none|zip|piz|b44|b44a|dwaa|dwab> [dwa-level]
//       Reads R,G,B HALF from <in.exr> and writes them with the given compression.
//   openexr-bench dump <in.exr> <out.raw> <half|f32rgb>
//       half: planar R,G,B HALF bits, little-endian. f32rgb: interleaved RGB f32 LE.
//   openexr-bench bench <threads> <min-ms> <min-samples> <max-samples> <in.exr>...
//       Decodes each file from memory, alternating arms each round, and prints
//       one CSV row per file and arm: file,threads,arm,samples,median_ns,p25_ns,p75_ns,min_ns,checksum
//       threads=1 runs in the calling thread (OpenEXR thread count 0).

#include <ImfChannelList.h>
#include <ImfFrameBuffer.h>
#include <ImfHeader.h>
#include <ImfIO.h>
#include <ImfInputFile.h>
#include <ImfOutputFile.h>
#include <ImfThreading.h>
#include <OpenEXRConfig.h>

#include <Iex.h>
#include <ImathBox.h>

#include <algorithm>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <fstream>
#include <iostream>
#include <iterator>
#include <stdexcept>
#include <string>
#include <thread>
#include <vector>

namespace
{

// In-memory stream with stateless positional reads, so OpenEXR's threaded
// reader takes the same lock-free path it uses for files.
class MemIStream : public Imf::IStream
{
public:
    MemIStream (const char* data, uint64_t size)
        : Imf::IStream ("memory"), _data (data), _size (size)
    {}

    bool isMemoryMapped () const override { return true; }

    bool read (char c[], int n) override
    {
        if (n < 0 || _pos + uint64_t (n) > _size)
            throw IEX_NAMESPACE::InputExc ("read past end of memory stream");
        std::memcpy (c, _data + _pos, size_t (n));
        _pos += uint64_t (n);
        return _pos < _size;
    }

    char* readMemoryMapped (int n) override
    {
        if (n < 0 || _pos + uint64_t (n) > _size)
            throw IEX_NAMESPACE::InputExc ("read past end of memory stream");
        char* p = const_cast<char*> (_data + _pos);
        _pos += uint64_t (n);
        return p;
    }

    uint64_t tellg () override { return _pos; }
    void     seekg (uint64_t pos) override { _pos = pos; }
    void     clear () override {}
    int64_t  size () override { return int64_t (_size); }
    bool     isStatelessRead () const override { return true; }

    int64_t read (void* buf, uint64_t sz, uint64_t offset) override
    {
        if (offset >= _size) return 0;
        uint64_t n = std::min (sz, _size - offset);
        std::memcpy (buf, _data + offset, size_t (n));
        return int64_t (n);
    }

private:
    const char* _data;
    uint64_t    _size;
    uint64_t    _pos = 0;
};

std::vector<char>
readFile (const std::string& path)
{
    std::ifstream in (path, std::ios::binary);
    if (!in) throw std::runtime_error ("cannot open " + path);
    return std::vector<char> (
        std::istreambuf_iterator<char> (in), std::istreambuf_iterator<char> ());
}

void
writeFile (const std::string& path, const void* data, size_t size)
{
    std::ofstream out (path, std::ios::binary);
    out.write (static_cast<const char*> (data), std::streamsize (size));
    if (!out) throw std::runtime_error ("cannot write " + path);
}

struct Planes
{
    int                   w = 0, h = 0;
    std::vector<uint16_t> data; // R plane, G plane, B plane
};

// Planar HALF R,G,B; output allocation is part of the measured decode.
Planes
decodeHalf (const std::vector<char>& file)
{
    MemIStream      s (file.data (), file.size ());
    Imf::InputFile  in (s, Imf::globalThreadCount ());
    Imath::Box2i    dw = in.header ().dataWindow ();
    Planes          p;
    p.w = dw.max.x - dw.min.x + 1;
    p.h = dw.max.y - dw.min.y + 1;
    size_t n = size_t (p.w) * size_t (p.h);
    p.data.assign (3 * n, 0);
    Imf::FrameBuffer fb;
    const char*      names[3] = {"R", "G", "B"};
    for (int c = 0; c < 3; ++c)
        fb.insert (
            names[c],
            Imf::Slice::Make (
                Imf::HALF, p.data.data () + c * n, dw, 2, 2 * size_t (p.w)));
    in.setFrameBuffer (fb);
    in.readPixels (dw.min.y, dw.max.y);
    return p;
}

// Interleaved RGB f32, the layout zenexr returns.
std::vector<float>
decodeF32Rgb (const std::vector<char>& file, int* wOut, int* hOut)
{
    MemIStream         s (file.data (), file.size ());
    Imf::InputFile     in (s, Imf::globalThreadCount ());
    Imath::Box2i       dw = in.header ().dataWindow ();
    int                w  = dw.max.x - dw.min.x + 1;
    int                h  = dw.max.y - dw.min.y + 1;
    std::vector<float> px (size_t (w) * size_t (h) * 3, 0.0f);
    Imf::FrameBuffer   fb;
    const char*        names[3] = {"R", "G", "B"};
    for (int c = 0; c < 3; ++c)
        fb.insert (
            names[c],
            Imf::Slice::Make (Imf::FLOAT, px.data () + c, dw, 12, 12 * size_t (w)));
    in.setFrameBuffer (fb);
    in.readPixels (dw.min.y, dw.max.y);
    if (wOut) *wOut = w;
    if (hOut) *hOut = h;
    return px;
}

Imf::Compression
parseCompression (const std::string& name)
{
    if (name == "none") return Imf::NO_COMPRESSION;
    if (name == "zip") return Imf::ZIP_COMPRESSION;
    if (name == "piz") return Imf::PIZ_COMPRESSION;
    if (name == "b44") return Imf::B44_COMPRESSION;
    if (name == "b44a") return Imf::B44A_COMPRESSION;
    if (name == "dwaa") return Imf::DWAA_COMPRESSION;
    if (name == "dwab") return Imf::DWAB_COMPRESSION;
    throw std::runtime_error ("unknown compression " + name);
}

int
cmdEncode (int argc, char** argv)
{
    if (argc < 5) throw std::runtime_error ("encode <in> <out> <compression> [level]");
    Imf::setGlobalThreadCount (int (std::thread::hardware_concurrency ()));
    std::vector<char> file = readFile (argv[2]);
    Planes            p    = decodeHalf (file);
    Imf::Header       header (p.w, p.h);
    header.compression () = parseCompression (argv[4]);
    if (argc > 5) header.dwaCompressionLevel () = std::stof (argv[5]);
    header.lineOrder () = Imf::INCREASING_Y;
    const char* names[3] = {"R", "G", "B"};
    size_t      n        = size_t (p.w) * size_t (p.h);
    Imf::FrameBuffer fb;
    for (int c = 0; c < 3; ++c)
    {
        header.channels ().insert (names[c], Imf::Channel (Imf::HALF));
        fb.insert (
            names[c],
            Imf::Slice (
                Imf::HALF,
                reinterpret_cast<char*> (p.data.data () + c * n),
                2,
                2 * size_t (p.w)));
    }
    Imf::OutputFile out (argv[3], header);
    out.setFrameBuffer (fb);
    out.writePixels (p.h);
    return 0;
}

int
cmdDump (int argc, char** argv)
{
    if (argc != 5) throw std::runtime_error ("dump <in> <out.raw> <half|f32rgb>");
    Imf::setGlobalThreadCount (0);
    std::vector<char> file = readFile (argv[2]);
    std::string       mode = argv[4];
    if (mode == "half")
    {
        Planes p = decodeHalf (file);
        writeFile (argv[3], p.data.data (), p.data.size () * 2);
    }
    else if (mode == "f32rgb")
    {
        std::vector<float> px = decodeF32Rgb (file, nullptr, nullptr);
        writeFile (argv[3], px.data (), px.size () * 4);
    }
    else
        throw std::runtime_error ("unknown dump mode " + mode);
    return 0;
}

struct Arm
{
    const char*           name;
    std::vector<uint64_t> ns;
    uint64_t              totalNs  = 0;
    uint64_t              checksum = 0;
};

uint64_t
runArm (int arm, const std::vector<char>& file)
{
    // The checksum keeps the decoded output observable.
    if (arm == 0)
    {
        Planes p = decodeHalf (file);
        return p.data[p.data.size () / 2] + p.data.back ();
    }
    int                w, h;
    std::vector<float> px = decodeF32Rgb (file, &w, &h);
    uint32_t           bits;
    std::memcpy (&bits, &px[px.size () / 2], 4);
    return bits;
}

uint64_t
percentile (std::vector<uint64_t> v, double q)
{
    std::sort (v.begin (), v.end ());
    size_t i = size_t (q * double (v.size () - 1) + 0.5);
    return v[i];
}

int
cmdBench (int argc, char** argv)
{
    if (argc < 7)
        throw std::runtime_error (
            "bench <threads> <min-ms> <min-samples> <max-samples> <file>...");
    int      threads    = std::stoi (argv[2]);
    uint64_t minNs      = uint64_t (std::stoull (argv[3])) * 1000000ull;
    size_t   minSamples = std::stoul (argv[4]);
    size_t   maxSamples = std::stoul (argv[5]);
    Imf::setGlobalThreadCount (threads <= 1 ? 0 : threads);
    uint64_t rng = 0x9e3779b97f4a7c15ull;
    for (int fi = 6; fi < argc; ++fi)
    {
        std::vector<char> file = readFile (argv[fi]);
        Arm               arms[2] = {{"openexr-f16-planar"}, {"openexr-f32-rgb"}};
        for (int a = 0; a < 2; ++a)
            arms[a].checksum = runArm (a, file); // warm-up
        for (;;)
        {
            bool done = true;
            for (auto& a: arms)
                if (a.ns.size () < minSamples ||
                    (a.totalNs < minNs && a.ns.size () < maxSamples))
                    done = false;
            if (done) break;
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            int order[2] = {int (rng & 1), int (1 - (rng & 1))};
            for (int a: order)
            {
                auto     t0 = std::chrono::steady_clock::now ();
                uint64_t c  = runArm (a, file);
                auto     t1 = std::chrono::steady_clock::now ();
                uint64_t ns = uint64_t (
                    std::chrono::duration_cast<std::chrono::nanoseconds> (t1 - t0)
                        .count ());
                arms[a].ns.push_back (ns);
                arms[a].totalNs += ns;
                arms[a].checksum ^= c;
            }
        }
        for (auto& a: arms)
            std::printf (
                "%s,%d,%s,%zu,%llu,%llu,%llu,%llu,%llu\n",
                argv[fi],
                threads,
                a.name,
                a.ns.size (),
                (unsigned long long) percentile (a.ns, 0.5),
                (unsigned long long) percentile (a.ns, 0.25),
                (unsigned long long) percentile (a.ns, 0.75),
                (unsigned long long) *std::min_element (a.ns.begin (), a.ns.end ()),
                (unsigned long long) a.checksum);
        std::fflush (stdout);
    }
    return 0;
}

} // namespace

int
main (int argc, char** argv)
{
    try
    {
        std::string cmd = argc > 1 ? argv[1] : "";
        if (cmd == "version")
        {
            std::printf ("OpenEXR %s\n", OPENEXR_VERSION_STRING);
            return 0;
        }
        if (cmd == "encode") return cmdEncode (argc, argv);
        if (cmd == "dump") return cmdDump (argc, argv);
        if (cmd == "bench") return cmdBench (argc, argv);
        std::fprintf (stderr, "usage: openexr-bench version|encode|dump|bench ...\n");
        return 2;
    }
    catch (const std::exception& e)
    {
        std::fprintf (stderr, "openexr-bench: %s\n", e.what ());
        return 1;
    }
}
