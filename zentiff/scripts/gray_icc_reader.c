#include <lcms2.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <tiffio.h>

int main(int argc, char **argv) {
    if (argc != 2) return 2;
    TIFF *file = TIFFOpen(argv[1], "r");
    if (!file) return 3;
    uint32_t width = 0, height = 0, profile_length = 0;
    uint16_t channels = 0, bits = 0, photometric = 0;
    void *profile_bytes = NULL;
    if (!TIFFGetField(file, TIFFTAG_IMAGEWIDTH, &width) ||
        !TIFFGetField(file, TIFFTAG_IMAGELENGTH, &height) ||
        !TIFFGetField(file, TIFFTAG_SAMPLESPERPIXEL, &channels) ||
        !TIFFGetField(file, TIFFTAG_BITSPERSAMPLE, &bits) ||
        !TIFFGetField(file, TIFFTAG_PHOTOMETRIC, &photometric) ||
        !TIFFGetField(file, TIFFTAG_ICCPROFILE, &profile_length, &profile_bytes)) return 4;
    if (width != 5 || height != 3 || bits != 8 || channels < 1 || channels > 2 ||
        photometric != PHOTOMETRIC_MINISBLACK) return 5;
    cmsHPROFILE profile = cmsOpenProfileFromMem(profile_bytes, profile_length);
    if (!profile || cmsGetColorSpace(profile) != cmsSigGrayData) return 6;
    cmsToneCurve *curve = cmsReadTag(profile, cmsSigGrayTRCTag);
    if (!curve || !cmsIsToneCurveMonotonic(curve)) return 7;
    uint8_t *row = malloc((size_t)TIFFScanlineSize(file));
    if (!row) return 8;
    for (uint32_t y = 0; y < height; ++y) {
        if (TIFFReadScanline(file, row, y, 0) < 0) return 9;
        for (uint32_t x = 0; x < width * channels; ++x) {
            uint8_t expected = (uint8_t)((y * width * channels + x) * 37);
            if (row[x] != expected) return 10;
        }
    }
    free(row);
    cmsCloseProfile(profile);
    TIFFClose(file);
    printf("GRAY ICC and exact samples: %s\n", argv[1]);
    return 0;
}
