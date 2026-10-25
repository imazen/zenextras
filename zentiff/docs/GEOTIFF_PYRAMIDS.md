# GeoTIFF, COG and pyramidal TIFF: where zentiff stands and what it needs

Survey and plan, 2026-10-20. Version and spec facts were checked on that date
against the linked sources; anything not checked says so. Statements about
zentiff, image-tiff and zencodec were checked against source at zenextras
`main` plus this change, the lilith/image-tiff fork at `da9f8647`, and zencodec
`87ec0d0`.

## 1. What zentiff does today

- Decodes **IFD0 only**. Later IFDs are walked to count pages and reported as
  `ImageSequence::Multi { image_count, random_access: true }`
  (`src/decode.rs` `count_pages`, `src/codec.rs` `tiff_info_to_image_info`).
- No page, SubIFD, level or region selection. `with_crop_hint` is not
  honoured and `push_decoder_from_source` is not implemented.
- Incremental decode is `WholeFileIncremental`: it re-probes a growing
  prefix and decodes once the whole file is present. It never asks for byte
  ranges.
- The inventory (`src/inventory/`) walks SubIFDs, EXIF, GPS and Interop
  directories and every strip and tile, but marks SubIFD images "not
  decoded".
- No GeoTIFF tags are read or written (33550, 33922, 34264, 34735–34737,
  42112, 42113). They survive only as unknown IFD0 entries the decode skips.
- The encoder writes **strips only**, through image-tiff, which has no tile
  writer.
- Published zentiff builds against crates.io `tiff` 0.11.3. Inside this
  workspace it builds against the lilith/image-tiff fork (root
  `Cargo.toml` `[patch.crates-io]`), which carries upstream's unreleased
  0.12 work plus fork fixes.

### Limits that bite large GeoTIFFs today

- IFD0's tag values: image-tiff now enforces `Limits::ifd_value_size` on
  out-of-line values (0.11.3 did not), and `Decoder::new` parses IFD0 before
  `with_limits`. The fork's `Decoder::new` reads IFD0 with the value limit
  raised to `decoding_buffer_size` (fork `2a7d124f`), so first images with
  more than 1 MiB of offsets open; `derive_tiff_limits` sets
  `ifd_value_size` for every later read.
- Whole-image decode only: a 100k × 100k RGB8 image needs 30 GB of output
  buffer, far past the defaults (`max_pixels` 120 M, `max_memory_bytes`
  4 GiB). Region and level decode are the only workable paths for such
  files.

## 2. The standards

| Document | Status | What matters for us |
|---|---|---|
| TIFF 6.0 ([PDF](https://download.osgeo.org/libtiff/doc/TIFF6.pdf)) | 1992 | Tiles (§15): sizes a multiple of 16, edge tiles padded, offsets row-major. NewSubfileType bits. |
| TIFF Technical Notes ([PageMaker 6 TN](https://download.osgeo.org/libtiff/doc/TIFFPM6.pdf)) | 1995 | SubIFDs (330), "thumbnails and other subsampled versions". |
| BigTIFF ([libtiff](https://libtiff.gitlab.io/libtiff/specification/bigtiff.html)) | de facto | 8-byte offsets; LONG8/SLONG8/IFD8. |
| OGC GeoTIFF 1.1, 19-008r4 ([spec](https://docs.ogc.org/is/19-008r4/19-008r4.html)) | 2019, current | GeoKey directory and the model-space tags. Draft keys (CoordinateEpoch, CelestialBody) are merged in the OGC repo; no 1.2 is published. |
| OGC COG 1.0, 21-026 ([spec](https://docs.ogc.org/is/21-026/21-026.html)) | 2023, current | Tiled IFDs; full resolution followed by reduced-resolution IFDs in the main chain (NewSubfileType bit 0); overviews carry no georeferencing; BigTIFF over 4 GB; HTTP range access. |

GeoTIFF essentials ([spec](https://docs.ogc.org/is/19-008r4/19-008r4.html)):

- GeoKeyDirectory (34735, SHORT) is mandatory. Keys sit inline or point
  into GeoDoubleParams (34736) or GeoAsciiParams (34737, `|`-terminated
  strings), in ascending key order.
- KeyRevision minor 0 means a 1.0 file and 1 means a 1.1 file. 1.1 renamed
  several keys (GeographicType → GeodeticCRS 2048, ProjectedCSType →
  ProjectedCRS 3072, …) without changing their numbers.
- Raster-to-model mapping: ModelPixelScale (33550) + ModelTiepoint (33922),
  or ModelTransformation (34264, a 4 × 4 matrix). Scale and transformation
  must not appear together. Several tiepoints without a scale are ground
  control points.
- GTRasterTypeGeoKey (1025): PixelIsArea or PixelIsPoint. PixelIsPoint puts
  the origin at the centre of pixel (0,0); GDAL shifts by half a pixel
  ([gtiffdataset_read.cpp](https://github.com/OSGeo/gdal/blob/master/frmts/gtiff/gtiffdataset_read.cpp)),
  tifffile does not. Match GDAL and test against it.
- Model space is always (x = east/lon, y = north/lat), whatever the EPSG
  axis order says.
- GDAL private tags: GDAL_NODATA (42113, ASCII, one value for all bands) and
  GDAL_METADATA (42112, XML)
  ([GTiff driver](https://gdal.org/en/stable/drivers/raster/gtiff.html)).

COG layout ([spec](https://docs.ogc.org/is/21-026/21-026.html) Rec 3, and the
[GDAL COG driver](https://gdal.org/en/stable/drivers/raster/cog.html)):

1. Header, then optionally GDAL's "ghost area" (`GDAL_STRUCTURAL_METADATA_SIZE=…`).
2. All IFDs: full resolution, its mask, the overviews, the mask overviews.
3. The TileOffsets/TileByteCounts arrays.
4. Tile data, smallest overview first, full resolution last, mask tiles
   interleaved.

A reader gets every IFD from one request at the start of the file.

### Pyramid conventions in the wild

| Convention | Levels live in | Notes |
|---|---|---|
| COG / GDAL | Main IFD chain, NewSubfileType 1 | Masks use 4, mask overviews 5. GDAL also reads SubIFD overviews of IFD0. |
| OME-TIFF 6 ([spec](https://ome-model.readthedocs.io/en/stable/ome-tiff/specification.html)) | SubIFDs of each full-resolution plane | Largest first, integer and isotropic downsampling. |
| libvips `tiffsave --pyramid` ([doc](https://www.libvips.org/API/current/method.Image.tiffsave.html)) | Main chain, or SubIFDs with `--subifd` | Default tile 128. |
| Aperio SVS ([OpenSlide](https://openslide.org/formats/aperio/)) | Main chain | IFD1 is a stripped thumbnail. Label and macro are stripped IFDs with NewSubfileType 1 / 9, so "reduced" does not always mean "level". |
| Hamamatsu NDPI ([OpenSlide](https://openslide.org/formats/hamamatsu/)) | Main chain | Classic TIFF with private 64-bit offset extensions past 4 GB; libtiff cannot open it. |
| Philips TIFF ([OpenSlide](https://openslide.org/formats/philips/)) | Main chain | Sparse tiles (offset 0, count 0) render white. |

Level selection, as the reference readers do it:

- **GDAL** ([rasterio.cpp](https://github.com/OSGeo/gdal/blob/master/gcore/rasterio.cpp)):
  the most reduced overview whose ratio stays below target × threshold
  (1.2 for nearest, 1.0 otherwise).
- **OpenSlide** ([openslide.c](https://github.com/openslide/openslide/blob/main/src/openslide.c)):
  the largest downsample at or below the request, with coordinates in
  level-0 space.

Both compute each level's scale from its actual dimensions, never as 2ⁿ,
because overview sizes round differently (GDAL rounds up).

Sparse tiles (offset 0 and count 0) mean nodata or 0 to GDAL, transparent to
OpenSlide's generic TIFF reader and white for Philips.

## 3. Reference implementations

| Implementation | Version (date) | Relevant capability |
|---|---|---|
| libtiff | 4.7.2 (2026-07-03) | The baseline; DNG JPEG XL code 52546 since 4.7.1. |
| GDAL | 3.13.3 (2026-08-18) | GTiff and COG drivers, `/vsicurl/` merged range reads, `gdal driver cog validate` (3.13), level selection, masks, nodata, overview generation. |
| tifffile | 2026.10.15 | `series.levels` over SubIFDs and the main chain, Zarr v3 store with NGFF multiscales, sparse-tile handling. |
| OpenSlide | 4.0.1 (2026-06-08) | Vendor pyramid detection and region reads in level-0 coordinates. |
| libvips | 8.18.7 (2026-09-26) | Pyramid writer (main chain or SubIFDs), `tiffload subifd=`. |
| rio-cogeo | 7.0.4 (2026-10-09) | COG writer and validator; 512 px tiles, power-of-2 overviews. |
| async-tiff (Rust) | 0.4.0 (2026-09-18) | Async tiled reads over object_store, merged tile requests, GeoKeys, masks/nodata; read-only. |
| geotiff-rust (Rust) | 0.9.0 (2026-10-18) | Its README claims windowed reads, overview discovery in the chain and SubIFDs, HTTP range COG access and a COG writer with ghost area; not verified. |
| image-tiff | 0.11.3, 0.12 unreleased | `read_chunk`, `read_directory(ptr)`, `TiffHeader`; strip-only encoder; GeoTIFF tag names but no GeoTIFF logic ([#98](https://github.com/image-rs/image-tiff/issues/98)). |

## 4. What zencodec already offers

From zencodec `87ec0d0` (public; `docs/range-decoding-contract.md` and
`docs/multi-image-design.md`):

- `IncrementalDecode::step(input, sink, stop)` with `requested() ->
  &[Request]`, where `Request::Range(ByteRange)` names exact spans. This is
  the "decoder says what it needs" dialogue; the contract's worked example is
  a remote TIFF crop (header, then IFD0 at 450 MB, then two coalesced tile
  ranges).
- `ReadAt` sources (`read_many_at`) and
  `DecodeJob::push_decoder_from_source(source, sink, preferred)` for seekable
  input. A codec advertises it with `with_seekable` and
  `with_access_pattern(RandomAccess)`.
- `zencodec-io` drivers coalesce range requests (`coalesce_byte_ranges`,
  `drive_from_read_at`). `zencodec-coroutine` lets a codec write the
  dialogue as straight-line async code over `read_many_at`.
- `with_crop_hint(x, y, w, h)` on the job; `OutputInfo::crop_applied`
  reports what was honoured.
- `Supplements::pyramid` (a flag) and `ImageSequence::Multi`.
- **Not in zencodec:** page, level or scale addressing.
  `MultiPageDecoder { decode_page }` and
  `PyramidDecoder { level_count, level_info, decode_level }` are sketches in
  `docs/multi-image-design.md`, explicitly not implemented. That doc routes
  GeoTIFF band and CRS metadata to the codec's native API.

## 5. Gaps, and a plan to close them

Ordered so that each step lands on its own with tests.

### Step 1: native IFD model (zentiff, no zencodec change)

- Classify every IFD reachable through the main chain and SubIFDs using
  zenmeta's IFD walk (already used by the inventory): NewSubfileType bits,
  tiled or stripped, dimensions, sample layout.
- Group IFDs into **images**, each with **levels** (main-chain reduced IFDs,
  as in COG/GDAL/libvips, or SubIFDs, as in OME/libvips `--subifd`),
  **masks**, and **associated images** (thumbnail, label, macro).
  Vendor-specific rules (Aperio, NDPI, Philips, QPTIFF) come after the
  generic ones.
- Each level's scale is its measured width/height ratio to level 0.
- Needs from image-tiff: decode an IFD reached by pointer (SubIFD levels).
  `read_directory(ptr)` and `read_directory_tags` read a directory and its
  tags but cannot decode it as an image; upstream PR [#284](https://github.com/image-rs/image-tiff/pull/284)
  (independent IFD/SubIFD seeking) is open. Land the equivalent in the fork
  first.
- Fixtures: COG (GDAL), libvips pyramids in the main chain and in SubIFDs,
  OME-TIFF, an Aperio-style label/macro file. Add them to codec-corpus.

### Step 2: GeoTIFF metadata (read)

- Parse the GeoKey directory (1.0 and 1.1 key names, inline/double/ASCII
  locations), the affine (tiepoint + scale, or the matrix), raster type
  (PixelIsPoint shift as GDAL does it), GCPs, GDAL_NODATA and GDAL_METADATA.
- Expose it on the native `TiffInfo` and keep the raw tags available for
  pass-through. CRS stays as EPSG code plus citation strings: resolving
  WKT/PROJ is out of scope for a codec.
- Pixel decode is unchanged: the georeferencing is metadata.
- Cross-check against GDAL (`gdalinfo -json`) and tifffile on a fixture set
  that covers PixelIsPoint, a ModelTransformation, a negative ScaleY, GCPs,
  NaN and integer nodata, and 1.0 vs 1.1 key revisions.

### Step 3: region and level decode

- Native API: `decode_region(level, rect)` into a caller-owned strided
  buffer. Intersect the rect with the tile grid, decode only those chunks
  (`read_chunk` / `read_chunk_bytes`), crop the padding off edge tiles, and
  fill sparse tiles with a policy value (0 or nodata, GDAL's default).
- zencodec: honour `with_crop_hint` at level 0 and report `crop_applied`.
  A level or scale choice needs a zencodec addition (a scale hint, or the
  `PyramidDecoder` sketch); propose that in zencodec rather than inventing
  a zentiff-only shape.
- Level selection follows GDAL's thresholds by default and also accepts an
  explicit level index.

### Step 4: range-driven (remote) reads

- image-tiff's `Decoder` needs `Read + Seek`, while zencodec's protocol is
  sans-I/O. A workable bridge:
  1. plan with zenmeta's walk: header, then IFDs (following pointers as they
     arrive), then the TileOffsets/TileByteCounts arrays;
  2. ask for exactly the tile ranges a region needs (`Request::Range`, which
     the driver coalesces);
  3. run image-tiff over a sparse `Read + Seek` view holding only the fetched
     ranges, where a read of an unfetched byte is an error, not a zero.
- Implement it as `push_decoder_from_source` (`with_seekable`,
  `AccessPattern::RandomAccess`) and as an `IncrementalDecode` through
  `zencodec-coroutine`.
- Prefetch the first 16 KiB in one request, as GDAL's `/vsicurl/` does; read
  GDAL's ghost area when present to size it.
- Measure request counts and bytes fetched against GDAL on the same COGs.

### Step 5: tiled, pyramidal and COG writing

- image-tiff has no tile writer. Because COG needs IFDs before data, zentiff
  should own the layout: compress each tile (through image-tiff's compressors
  or zen codecs), then write header, IFDs and offset arrays, then data
  smallest level first. Classic vs BigTIFF must be decided before writing
  (COG requires BigTIFF above 4 GB).
- Overviews from zenresize, with GDAL's rounding (round up) and a recorded
  filter. Overviews carry no georeferencing.
- GeoKeys written in ascending order with the revision set by the keys used.
- Optional GDAL ghost area and block leaders/trailers, so GDAL reports
  `LAYOUT=COG`.
- Validate with `gdal driver cog validate` (GDAL 3.13), tifffile and
  OpenSlide as test oracles.

### Step 6: codecs inside TIFF

- Decode coverage today, through image-tiff: none, LZW, Deflate (8 and
  32946), PackBits, CCITT Group 3 and 4, JPEG (zune-jpeg), WebP, ZSTD,
  SGILog/SGILog24, and in the fork CCITT RLE (2) and Group 3 2D (fork
  `19ab3adb`). Missing: LERC 34887 (+ tag 50674), LZMA 34925, JPEG XL 50002 (GDAL ≤ 3.10) and
  52546 (DNG 1.7; GDAL ≥ 3.11 writes this), JPEG 2000 33003/33005 (Aperio),
  DNG predictors 34892–34895.
- `Decoder::with_decompressor` (fork PR lilith/image-tiff#9, in review)
  takes a `ChunkDecompressor` per compression method. Its output replaces only
  decompression, so predictors and the rest of the pipeline still apply. With
  it, zenjpeg, zenwebp, zenjxl and zenjp2 can decode TIFF tiles, which closes
  the JXL and JPEG 2000 gaps with our own codecs. The March
  `feature/pluggable-decompressors` branch is superseded.

## 6. Open decisions

1. Native-API shape for images/levels/masks and the GeoTIFF struct. These are
   new public API, so they go through a reviewed PR.
2. Whether zencodec grows a level/scale hint or the `PyramidDecoder` trait,
   and whether a TIFF with SubIFD pyramids reports `Single` + `pyramid` (as
   `docs/multi-image-design.md` proposes) while a COG with main-chain
   overviews also reports `Single` rather than `Multi`.
3. Sparse-tile fill policy: 0, nodata or transparent.
4. How far to follow image-tiff upstream versus owning the TIFF structure
   layer in zentiff. Steps 4 and 5 already put layout in zentiff; image-tiff
   would remain the chunk decompressor.
