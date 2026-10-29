# Answer document fixtures

`<kind>.request.json` is a request body shaped as the broker's compose step shapes it, for a Word document, an Excel
workbook and PowerPoint slides. The content is invented: no research data, names or identifiers. It holds the cases a
writer has to keep straight: XML specials and quotes, a tab and a line break in one paragraph, a literal `_x0041_`,
formula-like text, numbers, an empty cell, a figure, and long digests. `src/broker/document_write_tests.rs` writes each
one and checks the file the way a reader resolves it.

Office's own reading is checked by hand when the writer changes:

1. Write each kind: `agent-sandbox-broker write-document < docx.request.json > document.docx` (and `xlsx`, `pptx`).
2. Read each back with the common Python readers and compare it with its request, cell by cell and in order:
   `pixi exec --spec python-docx --spec openpyxl --spec python-pptx python read_back.py docx.request.json document.docx`.
3. Open each in Word, Excel and PowerPoint (or export a PDF from each) and look at every page, sheet and slide: no
   repair prompt, the text and values where the request puts them, the figure, and the look the writer's module
   comments describe.
4. When the slide estimate (`src/broker/document_write_fit.rs`) or a slide's layout changes, write a deck at the edge
   of what the estimate accepts and look at every slide in PowerPoint: each text whole, inside its box and above the
   footer. `python3 slide_limits.py <agent-sandbox-broker> slide_limits.pptx` finds those edges with the writer
   itself, for each box and in CJK, Cyrillic, symbols and dingbats as well as Latin.
