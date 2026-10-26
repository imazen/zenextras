# zenpsd

Adobe Photoshop PSD (version 1) and PSB (version 2, Large Document Format)
decoder for the zen codec ecosystem.

`zenpsd` reads the embedded merged composite render in the PSD/PSB Image Data
section rather than compositing layers, while parsing the File Header, Color
Mode Data (including 8-bit indexed palettes), and Image Resources (ICC
profiles, EXIF, XMP, resolution info, version info, and alpha channel metadata).
