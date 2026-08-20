"""Give a font the leading for a row of `ratio` ems, split evenly.

egui has no line-height setting to reach for: epaint takes a row's height
straight from the face (`ascent - descent + leading`), and Inter ships no
leading at all, which sets prose at 1.21 em — tight for reading.

The extra goes into the ASCENT and DESCENT, half each, rather than into
the line gap. Gap is added below the baseline only, which reads fine for
a paragraph and wrongly for everything that centres itself in a line box:
egui_commonmark's bullets and list numbers floated a quarter of an em
above the text they belong to. Splitting it puts the baseline back in the
middle of its row.

Writing absolute values rather than adding to what is there keeps this
idempotent, so `gen-reading-font.sh` can be re-run at will — the original
metrics come from the un-inflated subset fonttools just wrote.

Usage: python set-leading.py <font.ttf> <ratio>
"""

import sys

from fontTools.ttLib import TTFont


def main() -> None:
    path, ratio = sys.argv[1], float(sys.argv[2])
    font = TTFont(path)
    upem = font["head"].unitsPerEm
    hhea, os2 = font["hhea"], font["OS/2"]
    # whichever of the two epaint's font backend reads, they now agree
    ascent = max(hhea.ascent, os2.sTypoAscender)
    descent = min(hhea.descent, os2.sTypoDescender)
    extra = max(0, round(ratio * upem) - (ascent - descent))
    ascent += extra // 2
    descent -= extra - extra // 2
    hhea.ascent, hhea.descent, hhea.lineGap = ascent, descent, 0
    os2.sTypoAscender, os2.sTypoDescender, os2.sTypoLineGap = ascent, descent, 0
    font.save(path)
    print(f"ascent {ascent}, descent {descent} — a row is {ratio} em, centred on its baseline")


if __name__ == "__main__":
    main()
