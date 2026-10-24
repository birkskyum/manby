---
title: tippecanoe's other programs
description: manby also runs tippecanoe-decode, tippecanoe-enumerate, tippecanoe-overzoom and tippecanoe-json-tool, so one program does all of tippecanoe's work.
---

manby also runs tippecanoe's smaller programs, as tippecanoe-rs has them, so one program does all of tippecanoe's work.

| tippecanoe | manby |
| --- | --- |
| `tippecanoe` | `manby` |
| `tile-join` | `manby join` |
| `tippecanoe-decode` | `manby decode` |
| `tippecanoe-enumerate` | `manby enumerate` |
| `tippecanoe-overzoom` | `manby overzoom` |
| `tippecanoe-json-tool` | `manby json-tool` |

Each program's own name works as the subcommand too (`manby tippecanoe-decode`), and under that name, through a link, manby is that program. So links named after all six make manby a drop-in replacement for tippecanoe.

```sh
for p in tippecanoe tile-join tippecanoe-decode tippecanoe-enumerate tippecanoe-overzoom tippecanoe-json-tool; do
  ln -s "$(which manby)" ~/.local/bin/$p
done
```

Only the tiling and the joining are manby's own, since that is where the time goes. The other four are tippecanoe-rs's code and run at its speed.

To tile an input file named like a subcommand, such as `decode`, write it as `./decode`.
