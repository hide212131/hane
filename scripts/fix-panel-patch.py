from pathlib import Path

path = Path("scripts/apply-panel-ui-patch.py")
text = path.read_text()
old = '''    """                        }))
                    .child(div().h(px(bottom_space))),
            ),
        );""",
    """                        }))
                    .child(div().h(px(bottom_space))),
            )
            .children(editor_scrollbar),
        );""",'''
new = '''    """                        }))
                        .child(div().h(px(bottom_space))),
                ),
        );""",
    """                        }))
                        .child(div().h(px(bottom_space))),
                )
                .children(editor_scrollbar),
        );""",'''
if old not in text:
    raise SystemExit("editor scrollbar replacement block not found")
path.write_text(text.replace(old, new, 1))
