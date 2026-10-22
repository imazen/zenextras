# exr decode benchmark summary (results-t8.csv,results-t8-rerun1.csv,results-t8-rerun2.csv, threads [8])

3965 file x arm values; 0 files dropped for contention.

## Geomean MP/s, OpenEXR-encoded inputs

Each cell is the geometric mean over images of pixels / median decode time.

### tiny 64x64

| compression | images | openexr-f16-planar | openexr-f32-rgb | exr-blocks-f16-planar-pool | exr-f16-planar | exr-f32-rgb |
|---|---|---|---|---|---|---|
| none | 16 | 65 | 63 | 186 | 395 | 297 |
| zip | 16 | 186 | 165 | 94 | 42 | 41 |
| piz | 16 | 19 | 18 | 6 | 6 | 5 |
| b44 | 16 | 108 | 92 | 124 | 45 | 44 |
| b44a | 16 | 86 | 72 | 124 | 45 | 44 |
| dwaa_15 | 16 | 16 | 16 | 8 | 7 | 7 |
| dwaa_45 | 16 | 17 | 16 | 9 | 8 | 8 |
| dwaa_150 | 16 | 17 | 17 | 12 | 10 | 10 |
| dwab_15 | 16 | 120 | 105 | 7 | 6 | 6 |
| dwab_45 | 16 | 136 | 117 | 8 | 6 | 6 |
| dwab_150 | 16 | 164 | 138 | 9 | 7 | 7 |

### small 256x256

| compression | images | openexr-f16-planar | openexr-f32-rgb | exr-blocks-f16-planar-pool | exr-f16-planar | exr-f32-rgb |
|---|---|---|---|---|---|---|
| none | 16 | 259 | 254 | 1146 | 1074 | 463 |
| zip | 16 | 311 | 269 | 468 | 300 | 269 |
| piz | 16 | 132 | 123 | 125 | 78 | 78 |
| b44 | 16 | 309 | 260 | 669 | 368 | 287 |
| b44a | 16 | 305 | 260 | 661 | 369 | 287 |
| dwaa_15 | 16 | 155 | 142 | 98 | 77 | 77 |
| dwaa_45 | 16 | 160 | 148 | 100 | 82 | 82 |
| dwaa_150 | 16 | 169 | 156 | 111 | 92 | 92 |
| dwab_15 | 16 | 108 | 83 | 37 | 27 | 26 |
| dwab_45 | 16 | 123 | 88 | 40 | 30 | 29 |
| dwab_150 | 16 | 180 | 123 | 47 | 35 | 33 |

### medium 0.24-1.1 MP

| compression | images | openexr-f16-planar | openexr-f32-rgb | exr-blocks-f16-planar-pool | exr-f16-planar | exr-f32-rgb |
|---|---|---|---|---|---|---|
| none | 17 | 954 | 740 | 1417 | 1216 | 507 |
| zip | 17 | 473 | 357 | 693 | 489 | 390 |
| piz | 17 | 404 | 330 | 244 | 198 | 188 |
| b44 | 17 | 924 | 604 | 1036 | 622 | 373 |
| b44a | 17 | 953 | 601 | 1062 | 622 | 395 |
| dwaa_15 | 17 | 444 | 341 | 220 | 181 | 174 |
| dwaa_45 | 17 | 505 | 383 | 254 | 204 | 192 |
| dwaa_150 | 17 | 640 | 451 | 313 | 239 | 221 |
| dwab_15 | 17 | 298 | 240 | 130 | 121 | 111 |
| dwab_45 | 17 | 353 | 277 | 153 | 141 | 130 |
| dwab_150 | 17 | 447 | 335 | 191 | 173 | 156 |

### large 4096x2048

| compression | images | openexr-f16-planar | openexr-f32-rgb | exr-blocks-f16-planar-pool | exr-f16-planar | exr-f32-rgb |
|---|---|---|---|---|---|---|
| none | 4 | 517 | 254 | 343 | 208 | 195 |
| zip | 4 | 313 | 191 | 484 | 195 | 194 |
| piz | 4 | 317 | 190 | 286 | 149 | 180 |
| b44 | 4 | 433 | 225 | 506 | 226 | 186 |
| b44a | 4 | 427 | 226 | 569 | 251 | 187 |
| dwaa_15 | 4 | 317 | 191 | 274 | 141 | 180 |
| dwaa_45 | 4 | 356 | 205 | 352 | 150 | 182 |
| dwaa_150 | 4 | 398 | 219 | 451 | 160 | 185 |
| dwab_15 | 4 | 279 | 180 | 246 | 129 | 133 |
| dwab_45 | 4 | 288 | 184 | 309 | 141 | 141 |
| dwab_150 | 4 | 289 | 185 | 359 | 149 | 144 |

## Speed ratio: Rust time / OpenEXR time (>1 means OpenEXR is faster)

Geomean over every OpenEXR-encoded file at that size of the per-file time ratio.

### tiny 64x64

| compression | exr-blocks-f16-planar-pool / openexr-f16-planar | exr-f16-planar / openexr-f16-planar | exr-f32-rgb / openexr-f32-rgb |
|---|---|---|---|
| none | 0.35x | 0.17x | 0.21x |
| zip | 1.98x | 4.42x | 4.05x |
| piz | 3.01x | 3.40x | 3.32x |
| b44 | 0.87x | 2.39x | 2.10x |
| b44a | 0.69x | 1.88x | 1.64x |
| dwaa_15 | 1.99x | 2.35x | 2.27x |
| dwaa_45 | 1.91x | 2.19x | 2.15x |
| dwaa_150 | 1.44x | 1.76x | 1.73x |
| dwab_15 | 17.41x | 20.30x | 17.86x |
| dwab_45 | 17.00x | 21.43x | 18.64x |
| dwab_150 | 19.29x | 23.18x | 19.64x |

### small 256x256

| compression | exr-blocks-f16-planar-pool / openexr-f16-planar | exr-f16-planar / openexr-f16-planar | exr-f32-rgb / openexr-f32-rgb |
|---|---|---|---|
| none | 0.23x | 0.24x | 0.55x |
| zip | 0.66x | 1.04x | 1.00x |
| piz | 1.06x | 1.68x | 1.58x |
| b44 | 0.46x | 0.84x | 0.90x |
| b44a | 0.46x | 0.83x | 0.91x |
| dwaa_15 | 1.57x | 2.00x | 1.85x |
| dwaa_45 | 1.60x | 1.95x | 1.80x |
| dwaa_150 | 1.52x | 1.84x | 1.70x |
| dwab_15 | 2.92x | 3.96x | 3.16x |
| dwab_45 | 3.07x | 4.08x | 3.05x |
| dwab_150 | 3.80x | 5.17x | 3.70x |

### medium 0.24-1.1 MP

| compression | exr-blocks-f16-planar-pool / openexr-f16-planar | exr-f16-planar / openexr-f16-planar | exr-f32-rgb / openexr-f32-rgb |
|---|---|---|---|
| none | 0.67x | 0.78x | 1.46x |
| zip | 0.68x | 0.97x | 0.92x |
| piz | 1.66x | 2.04x | 1.75x |
| b44 | 0.89x | 1.49x | 1.62x |
| b44a | 0.90x | 1.53x | 1.52x |
| dwaa_15 | 2.02x | 2.46x | 1.96x |
| dwaa_45 | 1.98x | 2.48x | 2.00x |
| dwaa_150 | 2.04x | 2.67x | 2.04x |
| dwab_15 | 2.29x | 2.46x | 2.16x |
| dwab_45 | 2.31x | 2.50x | 2.14x |
| dwab_150 | 2.33x | 2.58x | 2.16x |

### large 4096x2048

| compression | exr-blocks-f16-planar-pool / openexr-f16-planar | exr-f16-planar / openexr-f16-planar | exr-f32-rgb / openexr-f32-rgb |
|---|---|---|---|
| none | 1.51x | 2.48x | 1.30x |
| zip | 0.65x | 1.61x | 0.98x |
| piz | 1.11x | 2.12x | 1.06x |
| b44 | 0.86x | 1.92x | 1.21x |
| b44a | 0.75x | 1.70x | 1.21x |
| dwaa_15 | 1.16x | 2.25x | 1.07x |
| dwaa_45 | 1.01x | 2.36x | 1.13x |
| dwaa_150 | 0.88x | 2.48x | 1.18x |
| dwab_15 | 1.13x | 2.17x | 1.36x |
| dwab_45 | 0.93x | 2.04x | 1.30x |
| dwab_150 | 0.80x | 1.94x | 1.29x |

## Per-call time at 64x64 (geomean us, OpenEXR-encoded): the fixed cost

| compression | openexr-f16-planar | openexr-f32-rgb | exr-blocks-f16-planar-pool | exr-f16-planar | exr-f32-rgb |
|---|---|---|---|---|---|
| none | 63 | 65 | 22 | 10 | 14 |
| zip | 22 | 25 | 44 | 97 | 100 |
| piz | 219 | 225 | 658 | 744 | 745 |
| b44 | 38 | 45 | 33 | 90 | 94 |
| b44a | 48 | 57 | 33 | 90 | 94 |
| dwaa_15 | 251 | 258 | 500 | 590 | 587 |
| dwaa_45 | 245 | 252 | 468 | 537 | 540 |
| dwaa_150 | 237 | 243 | 340 | 416 | 421 |
| dwab_15 | 34 | 39 | 596 | 695 | 698 |
| dwab_45 | 30 | 35 | 513 | 647 | 654 |
| dwab_150 | 25 | 30 | 481 | 578 | 583 |

## Encoder effect: time(exr-encoded file) / time(OpenEXR-encoded file), same decoder

| compression | openexr-f16-planar | openexr-f32-rgb | exr-blocks-f16-planar-pool | exr-f16-planar | exr-f32-rgb |
|---|---|---|---|---|---|
| b44 | 1.02x (n=52) | 1.02x (n=52) | 1.00x (n=52) | 1.00x (n=52) | 1.00x (n=52) |
| b44a | 1.01x (n=52) | 1.00x (n=52) | 1.00x (n=52) | 1.01x (n=52) | 1.02x (n=52) |
| dwaa_45 | 0.98x (n=53) | 0.99x (n=53) | 0.98x (n=53) | 1.00x (n=53) | 1.00x (n=53) |
| dwab_45 | 1.01x (n=53) | 1.01x (n=53) | 1.01x (n=53) | 1.00x (n=53) | 1.00x (n=53) |

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
