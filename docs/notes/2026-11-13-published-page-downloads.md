## 2026-11-13, published pages download from /files (#758)

A page `nb publish` or MCP `notebook_publish` wrote into the server root
and opened through `/files` got `sandbox allow-scripts`, so its Download
notebook button did nothing; #755 had fixed only `/nbconvert/html`.
`get_file` now gives `PAGE_CSP` (`allow-downloads` added) to HTML whose
first 512 bytes hold `HTML_GENERATOR_MARK`, which `to_html` writes in the
head. Other HTML keeps jupyter_server's `sandbox allow-scripts`. A page
that copies the tag gains downloads only: it still runs in an opaque
origin, so it can't reach the UI's token or storage. Allowing downloads
for all of `/files` was the other choice; checking the tag keeps the
jupyter_server policy for everything else.
