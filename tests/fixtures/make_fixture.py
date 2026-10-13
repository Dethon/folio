"""Writes a small PDF with headings, body text, a list, links, an outline and two tables.

Run as: python3 make_fixture.py sample.pdf
"""
import sys

def text_ops(lines):
    out = ["BT"]
    for font, size, x, y, text in lines:
        text = text.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)")
        out.append(f"/{font} {size} Tf 1 0 0 1 {x} {y} Tm ({text}) Tj")
    out.append("ET")
    return "\n".join(out).encode("latin-1")

page1 = text_ops([
    ("F2", 24, 72, 720, "Sample Document"),
    ("F2", 16, 72, 680, "Introduction"),
    ("F1", 11, 72, 656, "This fixture exercises the text layer of the reader. It has"),
    ("F1", 11, 72, 642, "enough body text for the exporter to find the body size, a hyphen-"),
    ("F1", 11, 72, 628, "ated word, and a second paragraph below."),
    ("F1", 11, 72, 600, "The second paragraph is separated by vertical space."),
    ("F1", 11, 72, 572, "\x95 first item"),
    ("F1", 11, 72, 558, "\x95 second item"),
    ("F3", 10, 72, 530, "let answer = 42;"),
])
page2 = text_ops([
    ("F2", 16, 72, 720, "Results"),
    ("F1", 11, 72, 696, "The second page holds the closing remarks of the document."),
])

# Page 3: two tables. The first is ruled only horizontally, as typeset tables
# are, and its rules live in a form XObject placed with an offset, so reading
# them means composing transforms. The second is a grid of boxed cells, one of
# which wraps onto a second line.
page3 = text_ops([
    ("F1", 11, 72, 715, "Table 1: Scores by model."),
    ("F1", 11, 76, 687, "Model"), ("F1", 11, 200, 687, "Score"), ("F1", 11, 290, 687, "Cost"),
    ("F1", 11, 76, 667, "Small"), ("F1", 11, 200, 667, "27.3"), ("F1", 11, 290, 667, "3.3"),
    ("F1", 11, 76, 652, "Big net"), ("F1", 11, 200, 652, "28.4"), ("F1", 11, 290, 652, "2.3"),
    ("F1", 11, 77, 586, "Option"), ("F1", 11, 177, 586, "Description"),
    ("F3", 11, 77, 566, "-p"), ("F1", 11, 177, 566, "Show the patch introduced"),
    ("F1", 11, 177, 553, "with each commit."),
    ("F3", 11, 77, 534, "--stat"), ("F1", 11, 177, 534, "Show statistics."),
])
cells = [(72, 580, 100, 20), (172, 580, 228, 20), (72, 548, 100, 32), (172, 548, 228, 32),
         (72, 528, 100, 20), (172, 528, 228, 20)]
page3 += b"\n0.5 w\n" + b"\n".join(b"%d %d %d %d re S" % cell for cell in cells)
page3 += b"\nq 1 0 0 1 30 -40 cm /Fm1 Do Q"
# Drawn 30 left of and 40 above where the rules end up.
rules = b"0.8 w\n" + b"\n".join(b"42 %d m 330 %d l S" % (y + 40, y + 40) for y in (700, 682, 640))

objects = {}
objects[1] = b"<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R /PageMode /UseOutlines >>"
objects[2] = b"<< /Type /Pages /Kids [3 0 R 4 0 R 14 0 R] /Count 3 >>"
res = b"<< /Font << /F1 7 0 R /F2 8 0 R /F3 9 0 R >> >>"
objects[3] = (b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Resources " + res
              + b" /Annots [17 0 R 18 0 R 19 0 R 20 0 R 21 0 R] >>")
objects[4] = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources " + res + b" >>"
objects[5] = b"<< /Length %d >>\nstream\n" % len(page1) + page1 + b"\nendstream"
objects[6] = b"<< /Length %d >>\nstream\n" % len(page2) + page2 + b"\nendstream"
objects[7] = b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
objects[8] = b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>"
objects[9] = b"<< /Type /Font /Subtype /Type1 /BaseFont /Courier /Encoding /WinAnsiEncoding >>"
objects[10] = b"<< /Type /Outlines /First 11 0 R /Last 12 0 R /Count 2 >>"
objects[11] = b"<< /Title (Introduction) /Parent 10 0 R /Next 12 0 R /Dest [3 0 R /XYZ 72 700 0] >>"
objects[12] = b"<< /Title (Results) /Parent 10 0 R /Prev 11 0 R /Dest [4 0 R /XYZ 72 740 0] >>"
info = b"<< /Title (Sample Document) >>"
objects[13] = info
objects[14] = (b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 15 0 R /Resources "
               b"<< /Font << /F1 7 0 R /F2 8 0 R /F3 9 0 R >> /XObject << /Fm1 16 0 R >> >> >>")
objects[15] = b"<< /Length %d >>\nstream\n" % len(page3) + page3 + b"\nendstream"
objects[16] = (b"<< /Type /XObject /Subtype /Form /BBox [0 0 612 792] /Length %d >>\nstream\n" % len(rules)
               + rules + b"\nendstream")

# Links on the first page, none of them drawn. A hidden annotation of another
# kind comes first, so that a link's place among the annotations is not its
# place among the links. The last is one a reader must not follow.
link = b"<< /Type /Annot /Subtype /Link /Border [0 0 0] /Rect [%d %d %d %d] %s >>"
objects[17] = b"<< /Type /Annot /Subtype /Square /Rect [500 700 520 720] /F 2 >>"
objects[18] = link % (72, 653, 200, 667, b"/A << /S /URI /URI (https://example.com/folio?q=1) >>")
objects[19] = link % (72, 597, 200, 611, b"/Dest [4 0 R /XYZ 72 740 0]")
objects[20] = link % (72, 569, 140, 583, b"/A << /S /GoTo /D [14 0 R /Fit] >>")
objects[21] = link % (72, 555, 140, 569, b"/A << /S /Launch /F (cmd.exe) >>")

out = bytearray(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n")
offsets = {}
for number in sorted(objects):
    offsets[number] = len(out)
    out += b"%d 0 obj\n" % number + objects[number] + b"\nendobj\n"
xref = len(out)
out += b"xref\n0 %d\n" % (len(objects) + 1)
out += b"0000000000 65535 f \n"
for number in sorted(objects):
    out += b"%010d 00000 n \n" % offsets[number]
out += b"trailer\n<< /Size %d /Root 1 0 R /Info 13 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (len(objects) + 1, xref)
open(sys.argv[1], "wb").write(out)
