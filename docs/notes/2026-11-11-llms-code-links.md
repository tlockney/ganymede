## 2026-11-11, llms.txt copies leave links in code alone (#689)

The Markdown copies that llms.txt points to rewrite each link the way the
HTML does (#704). The rewrite also hit links inside code spans and code
blocks, which are examples of Markdown rather than links, so an example
such as `[Install](install.md)` came out pointing at GitHub. `rewriteLinks`
now skips fenced blocks and code spans. Found by the review of #704.
