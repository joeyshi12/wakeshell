Icons from [Lucide](https://lucide.dev) (lucide-static 1.61.0, ISC licence in `LICENSE.lucide`).
`stroke="currentColor"` is replaced with white; `src/icon.rs` uses them as masks and fills them
with the widget's CSS colour. Every `.svg` here is built into the binary.
To add one: download `https://unpkg.com/lucide-static@1.61.0/icons/<name>.svg` here and run
`sed -i 's/currentColor/#ffffff/' <name>.svg`.
