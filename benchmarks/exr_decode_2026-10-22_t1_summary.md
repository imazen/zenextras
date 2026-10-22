# exr decode benchmark summary (results-t1.csv, threads [1])

4758 file x arm values; 0 files dropped for contention.

## Geomean MP/s, OpenEXR-encoded inputs

Each cell is the geometric mean over images of pixels / median decode time.

### tiny 64x64

| compression | images | openexr-f16-planar | openexr-f32-rgb | exr-blocks-f16-planar | exr-f16-planar | exr-f32-rgb | zenexr-f32-rgb |
|---|---|---|---|---|---|---|---|
| none | 16 | 518 | 309 | 503 | 416 | 305 | 166 |
| zip | 16 | 110 | 97 | 106 | 103 | 92 | 73 |
| piz | 16 | 31 | 30 | 5 | 5 | 5 | 5 |
| b44 | 16 | 309 | 225 | 207 | 199 | 165 | 115 |
| b44a | 16 | 313 | 227 | 208 | 200 | 166 | 116 |
| dwaa_15 | 16 | 102 | 91 | 5 | 5 | 5 | 5 |
| dwaa_45 | 16 | 114 | 100 | 5 | 5 | 5 | 5 |
| dwaa_150 | 16 | 134 | 115 | 5 | 5 | 5 | 5 |
| dwab_15 | 16 | 122 | 106 | 9 | 9 | 9 | 9 |
| dwab_45 | 16 | 138 | 119 | 9 | 9 | 9 | 9 |
| dwab_150 | 16 | 167 | 140 | 10 | 10 | 9 | 9 |

### small 256x256

| compression | images | openexr-f16-planar | openexr-f32-rgb | exr-blocks-f16-planar | exr-f16-planar | exr-f32-rgb | zenexr-f32-rgb |
|---|---|---|---|---|---|---|---|
| none | 16 | 2052 | 557 | 1096 | 1071 | 467 | 137 |
| zip | 16 | 123 | 106 | 120 | 115 | 100 | 66 |
| piz | 16 | 75 | 63 | 23 | 23 | 22 | 21 |
| b44 | 16 | 414 | 267 | 254 | 242 | 151 | 93 |
| b44a | 16 | 422 | 271 | 256 | 245 | 151 | 94 |
| dwaa_15 | 16 | 99 | 82 | 22 | 22 | 21 | 19 |
| dwaa_45 | 16 | 116 | 94 | 24 | 24 | 22 | 21 |
| dwaa_150 | 16 | 148 | 116 | 27 | 26 | 25 | 23 |
| dwab_15 | 16 | 109 | 84 | 40 | 39 | 37 | 33 |
| dwab_45 | 16 | 125 | 88 | 43 | 42 | 40 | 36 |
| dwab_150 | 16 | 182 | 125 | 51 | 47 | 46 | 39 |

### medium 0.24-1.1 MP

| compression | images | openexr-f16-planar | openexr-f32-rgb | exr-blocks-f16-planar | exr-f16-planar | exr-f32-rgb | zenexr-f32-rgb |
|---|---|---|---|---|---|---|---|
| none | 17 | 2614 | 499 | 1297 | 1164 | 495 | 126 |
| zip | 17 | 127 | 105 | 125 | 118 | 101 | 64 |
| piz | 17 | 103 | 87 | 42 | 41 | 38 | 32 |
| b44 | 17 | 421 | 248 | 251 | 236 | 178 | 90 |
| b44a | 17 | 431 | 252 | 255 | 239 | 180 | 91 |
| dwaa_15 | 17 | 122 | 99 | 39 | 38 | 36 | 30 |
| dwaa_45 | 17 | 153 | 120 | 45 | 44 | 40 | 34 |
| dwaa_150 | 17 | 215 | 158 | 56 | 54 | 50 | 40 |
| dwab_15 | 17 | 124 | 101 | 47 | 47 | 41 | 36 |
| dwab_45 | 17 | 154 | 121 | 57 | 56 | 50 | 42 |
| dwab_150 | 17 | 218 | 160 | 73 | 71 | 62 | 51 |

### large 4096x2048

| compression | images | openexr-f16-planar | openexr-f32-rgb | exr-blocks-f16-planar | exr-f16-planar | exr-f32-rgb | zenexr-f32-rgb |
|---|---|---|---|---|---|---|---|
| none | 4 | 478 | 205 | 247 | 209 | 193 | 82 |
| zip | 4 | 115 | 88 | 118 | 99 | 85 | 53 |
| piz | 4 | 113 | 87 | 51 | 46 | 45 | 34 |
| b44 | 4 | 231 | 142 | 184 | 139 | 116 | 63 |
| b44a | 4 | 232 | 142 | 185 | 139 | 116 | 63 |
| dwaa_15 | 4 | 112 | 86 | 47 | 42 | 41 | 32 |
| dwaa_45 | 4 | 146 | 105 | 61 | 53 | 51 | 37 |
| dwaa_150 | 4 | 191 | 127 | 79 | 66 | 63 | 43 |
| dwab_15 | 4 | 110 | 83 | 49 | 43 | 41 | 32 |
| dwab_45 | 4 | 143 | 100 | 65 | 55 | 52 | 38 |
| dwab_150 | 4 | 184 | 120 | 84 | 67 | 63 | 44 |

## Speed ratio: Rust time / OpenEXR time (>1 means OpenEXR is faster)

Geomean over every OpenEXR-encoded file at that size of the per-file time ratio.

### tiny 64x64

| compression | exr-blocks-f16-planar / openexr-f16-planar | exr-f16-planar / openexr-f16-planar | exr-f32-rgb / openexr-f32-rgb | zenexr-f32-rgb / openexr-f32-rgb |
|---|---|---|---|---|
| none | 1.03x | 1.24x | 1.01x | 1.86x |
| zip | 1.04x | 1.07x | 1.05x | 1.33x |
| piz | 6.26x | 6.24x | 6.11x | 6.20x |
| b44 | 1.49x | 1.55x | 1.37x | 1.95x |
| b44a | 1.50x | 1.56x | 1.37x | 1.96x |
| dwaa_15 | 20.91x | 20.94x | 18.81x | 19.05x |
| dwaa_45 | 22.88x | 22.88x | 20.30x | 20.59x |
| dwaa_150 | 26.49x | 26.55x | 23.33x | 23.66x |
| dwab_15 | 13.24x | 13.23x | 11.97x | 12.28x |
| dwab_45 | 14.74x | 14.73x | 13.11x | 13.44x |
| dwab_150 | 17.29x | 17.30x | 15.02x | 15.43x |

### small 256x256

| compression | exr-blocks-f16-planar / openexr-f16-planar | exr-f16-planar / openexr-f16-planar | exr-f32-rgb / openexr-f32-rgb | zenexr-f32-rgb / openexr-f32-rgb |
|---|---|---|---|---|
| none | 1.87x | 1.92x | 1.19x | 4.06x |
| zip | 1.02x | 1.07x | 1.06x | 1.60x |
| piz | 3.31x | 3.25x | 2.81x | 2.99x |
| b44 | 1.63x | 1.71x | 1.77x | 2.86x |
| b44a | 1.65x | 1.72x | 1.79x | 2.89x |
| dwaa_15 | 4.52x | 4.53x | 3.95x | 4.24x |
| dwaa_45 | 4.91x | 4.91x | 4.19x | 4.52x |
| dwaa_150 | 5.50x | 5.59x | 4.63x | 5.00x |
| dwab_15 | 2.76x | 2.82x | 2.28x | 2.55x |
| dwab_45 | 2.89x | 2.96x | 2.20x | 2.47x |
| dwab_150 | 3.55x | 3.87x | 2.74x | 3.16x |

### medium 0.24-1.1 MP

| compression | exr-blocks-f16-planar / openexr-f16-planar | exr-f16-planar / openexr-f16-planar | exr-f32-rgb / openexr-f32-rgb | zenexr-f32-rgb / openexr-f32-rgb |
|---|---|---|---|---|
| none | 2.02x | 2.25x | 1.01x | 3.95x |
| zip | 1.02x | 1.07x | 1.03x | 1.63x |
| piz | 2.45x | 2.50x | 2.30x | 2.74x |
| b44 | 1.68x | 1.78x | 1.39x | 2.74x |
| b44a | 1.69x | 1.81x | 1.40x | 2.78x |
| dwaa_15 | 3.16x | 3.20x | 2.79x | 3.30x |
| dwaa_45 | 3.40x | 3.47x | 2.98x | 3.56x |
| dwaa_150 | 3.87x | 3.95x | 3.19x | 3.99x |
| dwab_15 | 2.62x | 2.65x | 2.44x | 2.78x |
| dwab_45 | 2.72x | 2.76x | 2.44x | 2.90x |
| dwab_150 | 2.98x | 3.06x | 2.56x | 3.14x |

### large 4096x2048

| compression | exr-blocks-f16-planar / openexr-f16-planar | exr-f16-planar / openexr-f16-planar | exr-f32-rgb / openexr-f32-rgb | zenexr-f32-rgb / openexr-f32-rgb |
|---|---|---|---|---|
| none | 1.93x | 2.28x | 1.06x | 2.51x |
| zip | 0.97x | 1.16x | 1.03x | 1.67x |
| piz | 2.20x | 2.44x | 1.93x | 2.57x |
| b44 | 1.25x | 1.66x | 1.23x | 2.24x |
| b44a | 1.25x | 1.67x | 1.23x | 2.25x |
| dwaa_15 | 2.37x | 2.64x | 2.08x | 2.69x |
| dwaa_45 | 2.38x | 2.74x | 2.05x | 2.80x |
| dwaa_150 | 2.41x | 2.90x | 2.02x | 2.93x |
| dwab_15 | 2.25x | 2.53x | 2.01x | 2.58x |
| dwab_45 | 2.21x | 2.60x | 1.93x | 2.61x |
| dwab_150 | 2.18x | 2.73x | 1.91x | 2.72x |

## Per-call time at 64x64 (geomean us, OpenEXR-encoded): the fixed cost

| compression | openexr-f16-planar | openexr-f32-rgb | exr-blocks-f16-planar | exr-f16-planar | exr-f32-rgb | zenexr-f32-rgb |
|---|---|---|---|---|---|---|
| none | 8 | 13 | 8 | 10 | 13 | 25 |
| zip | 37 | 42 | 39 | 40 | 44 | 56 |
| piz | 133 | 138 | 834 | 830 | 845 | 858 |
| b44 | 13 | 18 | 20 | 21 | 25 | 36 |
| b44a | 13 | 18 | 20 | 20 | 25 | 35 |
| dwaa_15 | 40 | 45 | 842 | 843 | 850 | 861 |
| dwaa_45 | 36 | 41 | 825 | 825 | 831 | 843 |
| dwaa_150 | 31 | 36 | 811 | 813 | 829 | 841 |
| dwab_15 | 34 | 38 | 445 | 445 | 460 | 472 |
| dwab_45 | 30 | 34 | 437 | 436 | 452 | 464 |
| dwab_150 | 24 | 29 | 423 | 423 | 440 | 452 |

## Encoder effect: time(exr-encoded file) / time(OpenEXR-encoded file), same decoder

| compression | openexr-f16-planar | openexr-f32-rgb | exr-blocks-f16-planar | exr-f16-planar | exr-f32-rgb | zenexr-f32-rgb |
|---|---|---|---|---|---|---|
| b44 | 1.01x (n=52) | 1.01x (n=52) | 1.00x (n=52) | 1.00x (n=52) | 1.00x (n=52) | 1.00x (n=52) |
| b44a | 1.01x (n=52) | 1.01x (n=52) | 1.00x (n=52) | 1.00x (n=52) | 1.00x (n=52) | 1.00x (n=52) |
| dwaa_45 | 0.99x (n=53) | 0.99x (n=53) | 1.00x (n=53) | 1.00x (n=53) | 1.00x (n=53) | 1.00x (n=53) |
| dwab_45 | 1.02x (n=53) | 1.01x (n=53) | 1.00x (n=53) | 1.01x (n=53) | 1.01x (n=53) | 1.01x (n=53) |

## Compressed size, OpenEXR encoder (geomean bits per pixel, RGB half = 48 bpp)

| compression | files | bpp |
|---|---|---|
| none | 53 | 48.98 |
| zip | 53 | 21.80 |
| piz | 53 | 22.84 |
| b44 | 53 | 21.23 |
| b44a | 53 | 19.78 |
| dwaa_15 | 53 | 14.32 |
| dwaa_45 | 53 | 9.80 |
| dwaa_150 | 53 | 5.63 |
| dwab_15 | 53 | 13.12 |
| dwab_45 | 53 | 8.86 |
| dwab_150 | 53 | 4.91 |
