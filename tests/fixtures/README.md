# Test fixtures

Every fixture here is **made up**. Acme Cloud, Fjordtech AS, Kari Nordmann
and every address, number and card are fictional. Never add a real receipt
or email to this folder: the repo is public.

| File | What it is |
|---|---|
| `invoice.html` | A SaaS invoice with seats, usage and VAT. The source of the others. |
| `invoice.pdf` | `invoice.html` printed to PDF by Chrome, so it has a text layer. |
| `scanned.pdf` | A PDF holding only an image of the invoice: no text layer. |
| `receipt.jpg` | A screenshot of the invoice. |
| `receipt.eml` | A receipt email: text and HTML bodies, an inline image, the PDF attached. |
| `forwarded.eml` | `receipt.eml` forwarded as an attachment, with a note. |
