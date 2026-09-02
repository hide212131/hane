from pathlib import Path

path = Path("scripts/apply-panel-ui-patch.py")
text = path.read_text()

replacements = [
    (
        '''    """                        }))
                    .child(div().h(px(bottom_space))),
            ),
        );""",
    """                        }))
                    .child(div().h(px(bottom_space))),
            )
            .children(editor_scrollbar),
        );""",''',
        '''    """                        }))
                        .child(div().h(px(bottom_space))),
                ),
        );""",
    """                        }))
                        .child(div().h(px(bottom_space))),
                )
                .children(editor_scrollbar),
        );""",''',
        "editor scrollbar replacement",
    ),
    (
        '''        });
        let mut draft_ids: Vec<SessionId> = self.work_folder_drafts.keys().copied().collect();''',
        '''        }).collect::<Vec<_>>();
        let mut draft_ids: Vec<SessionId> = self.work_folder_drafts.keys().copied().collect();''',
        "tree collection",
    ),
    (
        '''            });
        let content_height = sidebar_content_height(tree_row_count, draft_row_count);''',
        '''            }).collect::<Vec<_>>();
        let content_height = sidebar_content_height(tree_row_count, draft_row_count);''',
        "draft collection",
    ),
]

for old, new, label in replacements:
    if old not in text:
        raise SystemExit(f"{label} block not found")
    text = text.replace(old, new, 1)

path.write_text(text)
