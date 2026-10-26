"""Convert the user's Golden Eye emblem PNG into the Tauri icon set
(master, sizes, .ico) without adding any background or modification —
the source image is treated as the final design."""

from PIL import Image
from pathlib import Path

SRC = "/home/z/my-project/upload/Golden_Eye_Emblem_on_Dark_Luxe_Tile-removebg-preview.png"
OUT_DIR = Path("/home/z/my-project/download/lol-skin-launcher/src-tauri/icons")
OUT_DIR.mkdir(parents=True, exist_ok=True)


def main():
    src = Image.open(SRC).convert("RGBA")
    print(f"Source: {src.size[0]}x{src.size[1]} {src.mode}")

    # Master at 1024x1024 (Lanczos keeps the gold detail crisp)
    master = src.resize((1024, 1024), Image.LANCZOS)
    master.save(OUT_DIR / "icon.png", "PNG")

    # Standard PNG sizes Tauri wants
    for size, name in [
        (32, "32x32.png"),
        (128, "128x128.png"),
        (256, "128x128@2x.png"),
        (512, "store-icon.png"),
    ]:
        src.resize((size, size), Image.LANCZOS).save(OUT_DIR / name, "PNG")

    # Windows .ico with embedded sizes
    ico_sizes = [(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]
    master.save(OUT_DIR / "icon.ico", format="ICO", sizes=ico_sizes)

    print("OK icons written to", OUT_DIR)
    for f in sorted(OUT_DIR.iterdir()):
        print(f"  {f.name:25}  {f.stat().st_size:>8,} bytes")


if __name__ == "__main__":
    main()
