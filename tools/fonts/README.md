# Inter (the UI font)

ZooOS's desktop text is Inter 4.1 by Rasmus Andersson, under the SIL Open
Font License 1.1 (`Inter-OFL.txt`, copied from the release). The owner
approved the download on 2026-11-01 (docs/u1-design.md).

`xtask` renders the faces it needs into a bitmap atlas
(`xtask/src/font.rs`, put in the initrd as `ui/fonts.zfnt`). The font
files themselves are not committed.

## Getting it

| Item | Value |
| --- | --- |
| Release | https://github.com/rsms/inter/releases/tag/v4.1 (published 2024-11-16) |
| File | `Inter-4.1.zip`, 33,707,794 bytes |
| SHA-256 (computed here; the release publishes none) | `9883fdd4a49d4fb66bd8177ba6625ef9a64aa45899767dde3d36aa425756b11e` |

```sh
mkdir -p tools/fonts/dl tools/fonts/inter-4.1
gh release download v4.1 -R rsms/inter -p Inter-4.1.zip -D tools/fonts/dl
sha256sum tools/fonts/dl/Inter-4.1.zip
unzip -j tools/fonts/dl/Inter-4.1.zip extras/ttf/Inter-Regular.ttf \
  extras/ttf/Inter-Medium.ttf extras/ttf/Inter-SemiBold.ttf LICENSE.txt \
  -d tools/fonts/inter-4.1
```

The three files used:

| File | SHA-256 |
| --- | --- |
| `Inter-Regular.ttf` | `40d692fce188e4471e2b3cba937be967878f631ad3ebbbdcd587687c7ebe0c82` |
| `Inter-Medium.ttf` | `97ad806f526e41546d46365bb3a393145f75b7b1568913db74549ad8b8dba872` |
| `Inter-SemiBold.ttf` | `78a843fade9d4612a5567302fb595b56976eb5fcebf4fea5a5912d638bafcde3` |

Without these files `xtask` stops with a message pointing here.
