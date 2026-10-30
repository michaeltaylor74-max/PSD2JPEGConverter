# PSD to JPEG Converter

Batch converts Photoshop files to JPEG. Point it at a folder and it converts every `.psd` and `.psb` file in that folder **and all of its subfolders**, saving the JPEGs into a copy of the same folder layout somewhere else. Your original files are never touched.

```
Choose:  D:\Photos            →  save the copy in:  E:\Exports

D:\Photos\                       E:\Exports\Photos\
├── cover.psd                    ├── cover.jpg
├── 2024\                        ├── 2024\
│   └── Holiday\beach.psd        │   └── Holiday\beach.jpg
└── Clients\poster.psb           └── Clients\poster.jpg
```

## Download

Go to the **Releases** page and download the file for your computer (ignore the two "Source code" links, which GitHub adds automatically):

| Computer | File | First launch |
|---|---|---|
| Windows | `PSD-to-JPEG-Converter-Windows.exe` | Just double-click it. If SmartScreen appears, click *More info → Run anyway*. |
| Mac (Apple Silicon or Intel) | `PSD-to-JPEG-Converter-Mac.dmg` | Open it and drag the app into *Applications*. The first time, right-click the app → *Open* → *Open*. |
| Linux | `PSD-to-JPEG-Converter-Linux.tar.gz` | Extract it and run `./psd-to-jpeg-converter`. |

## Using it

1. **Folder to convert** – click *Choose…* (or drag a folder onto the window).
2. **Save the copy in** – choose where the converted folder should be created. A folder with the same name as the one you're converting is made there.
3. Set the **JPEG quality** (92 is a good default; 100 = largest files) and click **Convert**.

- Files are converted several at a time, using every CPU core.
- **Stop** finishes the files in progress, then stops. With *Skip files already converted* ticked, running it again carries on where it left off, so very large batches can be done in sessions.
- Anything that couldn't be converted, or needs checking, is listed in `conversion-problems.txt` inside the output folder (the *Open problem list* button opens it).

## What it handles

- PSD and PSB (large document format) files, any size up to JPEG's limit of 65,535 pixels per side
- 8, 16 and 32-bit images; RGB, CMYK, Grayscale, Duotone, Bitmap, Indexed and Lab colour
- Transparent areas become white (JPEG has no transparency)
- macOS `._` hidden files on external drives are ignored

### Good to know

- The converter uses the flattened preview Photoshop stores inside each file, which is exactly what Photoshop shows and very fast. If a layered file was saved with **Maximize Compatibility** turned off, that preview may be missing; the converter then tries to combine the layers itself and flags the file in the problem list so you can check it. If it can't, re-save that file in Photoshop with *Maximize PSD and PSB File Compatibility* set to *Always* (Preferences → File Handling).
- CMYK and Lab files are converted to RGB with a standard formula rather than your colour profile, so colours can differ slightly from Photoshop's own export.
- The JPEGs don't carry an embedded colour profile and are treated as sRGB.

## Command line

Running the program with arguments skips the window:

```
psd-to-jpeg-converter <SOURCE FOLDER> <DESTINATION FOLDER> [--quality 92] [--overwrite] [--threads N]
```

## Building from source

Install Rust from <https://rustup.rs>, then:

```
cargo build --release
```

The program is at `target/release/psd-to-jpeg-converter` (`.exe` on Windows). Pushing a tag such as `v1.0.1` makes GitHub build and publish the Windows, Mac and Linux downloads automatically.
