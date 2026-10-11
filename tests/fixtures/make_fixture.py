"""Writes a small two-page PDF with headings, body text, a list and an outline."""
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

objects = {}
objects[1] = b"<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R /PageMode /UseOutlines >>"
objects[2] = b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"
res = b"<< /Font << /F1 7 0 R /F2 8 0 R /F3 9 0 R >> >>"
objects[3] = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Resources " + res + b" >>"
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
