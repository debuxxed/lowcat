use super::*;

#[derive(Clone, Copy)]
struct RowRenderLayout<'a> {
    keys: &'a [String],
    tag_widths: &'a [Pixels],
    tag_key_action_width: Pixels,
}

impl FileTable {
    fn render_rows(
        &mut self,
        range: Range<usize>,
        keys: Vec<String>,
        tag_widths: Vec<Pixels>,
        tag_key_action_width: Pixels,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        crate::perf::sample("table.rows.rate");
        let rows_start = crate::perf::start();
        let selected_actions = self.cached_selected_rows_actions(cx);
        let (records, total_rows, visible_start, visible_end) = {
            let state = self.library.read(cx).active_state();
            let total_rows = state.results.len();
            let visible_start = range.start.min(total_rows);
            let visible_end = range.end.min(total_rows).max(visible_start);

            (
                state.results[visible_start..visible_end].to_vec(),
                total_rows,
                visible_start,
                visible_end,
            )
        };

        let rows = records
            .iter()
            .enumerate()
            .map(|(offset, record)| {
                self.render_record(
                    visible_start + offset,
                    record,
                    RowRenderLayout {
                        keys: &keys,
                        tag_widths: &tag_widths,
                        tag_key_action_width,
                    },
                    selected_actions.as_ref(),
                    cx,
                )
            })
            .collect();

        crate::perf::finish("table.rows", rows_start, || {
            format!(
                "visible={}..{} total={} keys={} selected={}",
                visible_start,
                visible_end,
                total_rows,
                keys.len(),
                self.selected.len()
            )
        });
        rows
    }

    fn render_record(
        &self,
        row_ix: usize,
        record: &FileRecord,
        layout: RowRenderLayout<'_>,
        selected_actions: Option<&Arc<SelectedRowsActions>>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let path = record.path.clone();
        let convertible = record.is_convertible();
        let selected = self.selected.contains(path.as_path());
        let preview_active = self.preview_active_row.as_ref() == Some(&path);
        let waveform = record.primary_waveform().copied();
        let playhead_ratio = preview_active.then(|| {
            self.preview_scrub
                .as_ref()
                .filter(|scrub| scrub.path == path)
                .map(|scrub| scrub.ratio)
                .or_else(|| self.library.read(cx).preview_playhead_ratio_for_path(&path))
        });
        let playhead_ratio = playhead_ratio.flatten();
        if preview_active {
            Self::store_preview_playhead(&self.preview_playhead_bits, playhead_ratio);
        }
        let multi_selection = selected
            .then_some(selected_actions)
            .flatten()
            .filter(|_| self.selected.len() > 1);
        let delete_target = multi_selection
            .map(|actions| actions.delete_target.clone())
            .unwrap_or_else(|| Self::record_delete_target(record));
        let rename_target = multi_selection
            .map(|actions| actions.rename_target.clone())
            .unwrap_or_else(|| Self::record_rename_target(record));
        let row_hover_bg = if convertible {
            hsla(0.095, 1., 0.55, 0.2)
        } else {
            cx.theme().table_hover
        };
        let convertible_bg = hsla(0.095, 1., 0.55, 0.12);
        let chip_delete_bg = red().opacity(0.18);
        let row_delete_bg = red().opacity(0.18);
        let format_chip_hovered = self.hovered_format_chip.as_ref().is_some_and(|hovered| {
            record
                .variants
                .iter()
                .any(|variant| &variant.path == hovered)
        });
        let row_hovered = self.hovered_row.as_ref() == Some(&path);
        let row_delete_hover_enabled = self.hovered_tag_chip.is_none() && !format_chip_hovered;
        let delete_hovered = row_delete_hover_enabled
            && self.alt_down
            && self.hovered_delete_row.as_ref().is_some_and(|hovered| {
                if self.selected.len() > 1 && self.selected.contains(hovered.as_path()) {
                    selected
                } else {
                    hovered == &path
                }
            });
        let open_path = record.path.clone();
        let open_preview_active = preview_active;
        let select_path = record.path.clone();
        let pending_drag_name = record.name.clone();
        let hover_path = record.path.clone();
        let delete_click_target = delete_target.clone();
        let conversion_actions = multi_selection
            .map(|actions| actions.conversion_actions.clone())
            .unwrap_or_else(|| Self::record_conversion_actions(record));
        let table = cx.entity();
        let row_drag_paths = multi_selection
            .map(|actions| actions.row_drag_paths.clone())
            .unwrap_or_else(|| Arc::new(vec![record.path.clone()]));
        let row_drag = InternalFileDrag::new_shared(record.name.clone(), row_drag_paths);
        let row_drag_for_mouse_down = row_drag.clone();
        let table_for_drag = table.clone();
        let table_for_context_menu = table.clone();
        let menu_action_context = self.focus_handle.clone();
        let menu_record = record.clone();
        let row_group = SharedString::from(format!("row-group:{}", path.display()));
        let mut row = div()
            .id(SharedString::from(format!("row:{}", path.display())))
            .group(row_group.clone())
            .h(ROW_HEIGHT)
            .flex()
            .flex_row()
            .w_full()
            .relative()
            .overflow_hidden()
            .text_sm()
            .when(row_ix > 0, |s| {
                s.border_t_1().border_color(cx.theme().table_row_border)
            })
            .when(convertible, |s| s.bg(convertible_bg))
            .when(selected || row_hovered, |s| s.bg(row_hover_bg))
            .when(delete_hovered, |s| s.bg(row_delete_bg))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                this.set_row_hovered(hover_path.clone(), *hovered, cx);
            }))
            .on_click(cx.listener(move |_, event: &ClickEvent, _, _| {
                if open_preview_active {
                    return;
                }
                if event.modifiers().alt {
                    return;
                }
                if event.click_count() == 2 {
                    open_in_default_app(&open_path);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    if event.click_count == 1 {
                        if event.modifiers.alt {
                            this.clear_pending_drag();
                            this.confirm_delete_target(
                                delete_click_target.as_ref().clone(),
                                window,
                                cx,
                            );
                            cx.stop_propagation();
                            return;
                        }
                        if event.modifiers.shift || !this.selected.contains(&select_path) {
                            this.select_row(select_path.clone(), event.modifiers.shift, window, cx);
                        }
                        this.start_pending_row_drag(
                            pending_drag_name.clone(),
                            &select_path,
                            row_drag_for_mouse_down.clone(),
                            cx,
                        );
                    }
                }),
            )
            .when(!self.alt_down, |row| {
                row.on_drag(row_drag, move |drag, cursor_offset, window, cx| {
                    table_for_drag.update(cx, |this, cx| {
                        this.begin_internal_file_drag(drag, window, cx);
                    });
                    cx.new(|_| FileDragPreview::new(drag, cursor_offset))
                })
            })
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, window, cx| {
                    this.finish_local_file_drag(window, cx);
                }),
            )
            .context_menu(move |menu, window, menu_cx| {
                let table = table_for_context_menu.clone();
                let actions = conversion_actions.clone();
                let target = delete_target.clone();
                let row_rename_target = rename_target.clone();
                let row_count = target.row_count;
                menu_cx.update_entity(&table, |this, cx| {
                    this.row_context_menu_open = true;
                    cx.notify();
                });
                let popup = menu_cx.entity().clone();
                window
                    .subscribe(&popup, menu_cx, {
                        let table = table.clone();
                        move |_, _: &DismissEvent, window, cx| {
                            cx.update_entity(&table, |this, cx| {
                                this.row_context_menu_open = false;
                                this.confirm_pending_context_menu_tag_rename(window, cx);
                                this.confirm_pending_context_menu_rename(window, cx);
                                this.confirm_pending_context_menu_delete(window, cx);
                                cx.notify();
                            });
                        }
                    })
                    .detach();
                let mut menu = menu
                    .max_w(px(CONVERT_MENU_PANE_WIDTH))
                    .action_context(menu_action_context.clone());
                let (format_context_target, tag_context_target) =
                    menu_cx.update_entity(&table, |this, _| {
                        let format_context_target =
                            this.hovered_format_chip.as_ref().and_then(|hovered| {
                                unique_format_variants(&menu_record)
                                    .into_iter()
                                    .find(|variant| variant.path == *hovered)
                                    .map(|variant| {
                                        (
                                            variant.extension.clone(),
                                            this.format_delete_target(
                                                &menu_record,
                                                &variant.extension,
                                            ),
                                        )
                                    })
                            });
                        let tag_context_target =
                            this.hovered_tag_chip.as_ref().and_then(|target| {
                                if target.path == menu_record.path
                                    && menu_record
                                        .tags
                                        .get(&target.key)
                                        .is_some_and(|values| values.contains(&target.value))
                                {
                                    Some(target.clone())
                                } else {
                                    None
                                }
                            });
                        (format_context_target, tag_context_target)
                    });
                if let Some((extension, target)) = format_context_target {
                    let table = table.clone();
                    let label =
                        SharedString::from(format!("Delete {}", extension.to_ascii_uppercase()));
                    return menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                        cx.update_entity(&table, |this, _| {
                            this.request_context_menu_delete(target.clone());
                        });
                    }));
                }
                if let Some(target) = tag_context_target {
                    let remove_table = table.clone();
                    let rename_table = table.clone();
                    let rename_all_table = table.clone();
                    let label = if target.value.is_empty() {
                        SharedString::from("Remove Tag")
                    } else {
                        SharedString::from(format!("Remove {}", target.value))
                    };
                    let rename_target = target.clone();
                    let rename_all_target = RenameTarget {
                        kind: RenameKind::TagAll {
                            key: target.key.clone(),
                            old_value: target.value.clone(),
                        },
                    };
                    return menu
                        .item(PopupMenuItem::new("Rename").on_click(move |_, _, cx| {
                            cx.update_entity(&rename_table, |this, _| {
                                this.request_context_menu_tag_rename(rename_target.clone());
                            });
                        }))
                        .item(PopupMenuItem::new("Rename all").on_click(move |_, _, cx| {
                            cx.update_entity(&rename_all_table, |this, _| {
                                this.request_context_menu_rename(rename_all_target.clone());
                            });
                        }))
                        .separator()
                        .item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                            cx.update_entity(&remove_table, |this, cx| {
                                this.remove_tag_from_target(
                                    &target.path,
                                    &target.key,
                                    &target.value,
                                    window,
                                    cx,
                                );
                            });
                        }));
                }
                for action in actions.iter().cloned() {
                    let table = table.clone();
                    let target = action.target;
                    let label = SharedString::from(format!("Convert to {}", target.label()));
                    menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                        let sources = action.sources.clone();
                        cx.update_entity(&table, |this, cx| {
                            this.library.update(cx, |lib, cx| {
                                lib.convert_files_to_format(sources, target, cx);
                            });
                        });
                    }));
                }
                if !conversion_actions.is_empty() {
                    menu = menu.separator();
                }
                let rename_table = table.clone();
                menu = menu.item(PopupMenuItem::new("Rename").on_click(move |_, _, cx| {
                    cx.update_entity(&rename_table, |this, _| {
                        this.request_context_menu_rename(row_rename_target.as_ref().clone());
                    });
                }));
                let delete_table = table.clone();
                let label = if row_count == 1 {
                    SharedString::from("Delete")
                } else {
                    SharedString::from(format!("Delete {row_count} Rows"))
                };
                menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                    cx.update_entity(&delete_table, |this, _| {
                        this.request_context_menu_delete(target.as_ref().clone());
                    });
                }));
                menu
            })
            .when(preview_active, |row| {
                row.child(div().absolute().inset_0().child(preview_waveform::element(
                    SharedString::from(format!("preview-waveform:{}", path.display())).into(),
                    cx.entity(),
                    path.clone(),
                    waveform,
                    self.preview_playhead_bits.clone(),
                )))
            })
            .when(!preview_active, |row| {
                row.child(
                    div()
                        .h_full()
                        .h_flex()
                        .items_center()
                        .flex_1()
                        .min_w_0()
                        .px(CONTENT_PX)
                        .py(px(4.))
                        .gap_1()
                        .overflow_hidden()
                        .child(
                            div()
                                .flex_shrink(1.)
                                .min_w_0()
                                .truncate()
                                .child(record.name.clone()),
                        )
                        .child(
                            div()
                                .h_flex()
                                .items_center()
                                .gap_1()
                                .flex_shrink_0()
                                .children(unique_format_variants(record).into_iter().map(
                                    |variant| {
                                        let record = record.clone();
                                        let extension = variant.extension.clone();
                                        let label = extension.clone();
                                        let variant_path = variant.path.clone();
                                        let format_drag_paths = multi_selection
                                            .and_then(|actions| {
                                                actions
                                                    .format_drag_paths
                                                    .get(&extension.to_ascii_lowercase())
                                            })
                                            .cloned()
                                            .unwrap_or_else(|| {
                                                Arc::new(vec![variant_path.clone()])
                                            });
                                        let format_drag = InternalFileDrag::new_shared(
                                            format!(".{extension}"),
                                            format_drag_paths,
                                        );
                                        let format_drag_for_mouse_down = format_drag.clone();
                                        let table_for_format_drag = table.clone();
                                        let format_delete_target =
                                            self.format_delete_target(&record, &extension);
                                        let chip_bg = if self.alt_down
                                            && self.hovered_format_chip.as_ref()
                                                == Some(&variant_path)
                                        {
                                            chip_delete_bg
                                        } else {
                                            cx.theme().muted
                                        };
                                        div()
                                    .id(SharedString::from(format!(
                                        "extension-chip:{}:{extension}",
                                        record.path.display()
                                    )))
                                    .h(px(18.))
                                    .min_w(px(26.))
                                    .px_1()
                                    .flex_shrink_0()
                                    .h_flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_md()
                                    .text_size(px(10.))
                                    .bg(chip_bg)
                                    .text_color(cx.theme().muted_foreground)
                                    .cursor_pointer()
                                    .child(SharedString::from(label))
                                    .on_hover(cx.listener({
                                        let variant_path = variant_path.clone();
                                        move |this, hovered: &bool, _, cx| {
                                            this.set_format_chip_hovered(
                                                variant_path.clone(),
                                                *hovered,
                                                cx,
                                            );
                                        }
                                    }))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener({
                                            let variant_path = variant_path.clone();
                                            move |this, event: &MouseDownEvent, window, cx| {
                                                if event.click_count == 1 {
                                                    if event.modifiers.alt {
                                                        this.clear_pending_drag();
                                                        this.confirm_delete_target(
                                                            format_delete_target.clone(),
                                                            window,
                                                            cx,
                                                        );
                                                        debug_table_interaction(|| {
                                                            format!(
                                                                "format delete requested path={}",
                                                                variant_path.display()
                                                            )
                                                        });
                                                        cx.stop_propagation();
                                                        return;
                                                    }
                                                    if event.modifiers.shift
                                                        || !this.selected.contains(&record.path)
                                                    {
                                                        this.select_row(
                                                            record.path.clone(),
                                                            event.modifiers.shift,
                                                            window,
                                                            cx,
                                                        );
                                                    }
                                                    this.start_pending_format_drag(
                                                        &record,
                                                        extension.clone(),
                                                        format_drag_for_mouse_down.clone(),
                                                        cx,
                                                    );
                                                }
                                                cx.stop_propagation();
                                            }
                                        }),
                                    )
                                    .on_mouse_down(
                                        MouseButton::Right,
                                        cx.listener({
                                            let variant_path = variant_path.clone();
                                            move |this, _: &MouseDownEvent, _, cx| {
                                                this.set_format_chip_hovered(
                                                    variant_path.clone(),
                                                    true,
                                                    cx,
                                                );
                                            }
                                        }),
                                    )
                                    .when(!self.alt_down, |chip| {
                                        chip.on_drag(
                                            format_drag,
                                            move |drag, cursor_offset, window, cx| {
                                                table_for_format_drag.update(
                                                    cx,
                                                    |this, cx| {
                                                        this.begin_internal_file_drag(
                                                            drag, window, cx,
                                                        );
                                                    },
                                                );
                                                cx.new(|_| {
                                                    FileDragPreview::new(drag, cursor_offset)
                                                })
                                            },
                                        )
                                    })
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(|this, _: &MouseUpEvent, window, cx| {
                                            this.finish_local_file_drag(window, cx);
                                            cx.stop_propagation();
                                        }),
                                    )
                                    },
                                )),
                        ),
                )
            });

        for (key, tag_width) in layout.keys.iter().zip(layout.tag_widths) {
            let group = SharedString::from(format!("cell:{}:{key}", path.display()));

            let mut cell = div()
                .id(group.clone())
                .group(group.clone())
                .absolute()
                .left(CONTENT_PX)
                .right(px(0.))
                .top_0()
                .bottom_0()
                .h_flex()
                .flex_nowrap()
                .items_center()
                .gap_1();

            if !preview_active && let Some(values) = record.tags.get(key) {
                for value in values {
                    let (key, value, path) = (key.clone(), value.clone(), path.clone());
                    let tag_target = TagChipTarget {
                        path: path.clone(),
                        key: key.clone(),
                        value: value.clone(),
                    };
                    let chip_group =
                        SharedString::from(format!("chip-group:{}:{key}:{value}", path.display()));
                    let is_renaming =
                        Self::editing_is_rename_value(self.editing.as_ref(), &path, &key, &value);
                    let chip_width =
                        px(value.chars().count() as f32 * TAG_TEXT_WIDTH
                            + TAG_CHIP_X_PADDING_WIDTH);

                    let mut chip = div()
                        .id(SharedString::from(format!(
                            "chip:{}:{key}:{value}",
                            path.display()
                        )))
                        .group(chip_group.clone())
                        .relative()
                        .h(ROW_HEIGHT)
                        .h_flex()
                        .items_center();

                    if is_renaming {
                        chip = chip.child(
                            div()
                                .w(chip_width)
                                .min_w(chip_width)
                                .h_flex()
                                .items_center()
                                .flex_shrink_0()
                                .px_1p5()
                                .rounded_md()
                                .text_xs()
                                .bg(cx.theme().muted)
                                .overflow_hidden()
                                .on_key_down(cx.listener(
                                    |this, event: &KeyDownEvent, window, cx| {
                                        if event.keystroke.key == "escape" {
                                            this.cancel_tag(window, cx);
                                            cx.stop_propagation();
                                        }
                                    },
                                ))
                                .child(
                                    Input::new(&self.tag_input)
                                        .appearance(false)
                                        .xsmall()
                                        .px_0()
                                        .flex_1()
                                        .mr(px(-10.))
                                        .min_w_0(),
                                ),
                        );
                    } else {
                        let rename_target = tag_target.clone();
                        chip = chip
                            .child(
                                div()
                                    .px_1p5()
                                    .rounded_md()
                                    .text_xs()
                                    .h(px(18.))
                                    .h_flex()
                                    .items_center()
                                    .whitespace_nowrap()
                                    .bg(cx.theme().muted)
                                    .text_color(cx.theme().muted_foreground)
                                    .child(SharedString::from(value.clone()))
                                    .when(self.alt_down, |this| {
                                        this.group_hover(chip_group, move |this| {
                                            this.bg(chip_delete_bg)
                                        })
                                    }),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!(
                                        "chip-hitbox:{}:{key}:{value}",
                                        path.display()
                                    )))
                                    .absolute()
                                    .left_0()
                                    .right_0()
                                    .top(px(-1.))
                                    .bottom(px(-1.))
                                    .bg(cx.theme().transparent)
                                    .cursor_pointer()
                                    .on_hover(cx.listener({
                                        let tag_target = tag_target.clone();
                                        move |this, hovered: &bool, _, cx| {
                                            this.set_tag_chip_hovered(
                                                tag_target.clone(),
                                                *hovered,
                                                cx,
                                            );
                                        }
                                    }))
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .on_mouse_up(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .on_mouse_move(|_, _, cx| cx.stop_propagation())
                                    .on_click(cx.listener(
                                        move |this, event: &ClickEvent, window, cx| {
                                            let modifiers = event.modifiers();
                                            if modifiers.alt {
                                                this.remove_tag_from_target(
                                                    &path, &key, &value, window, cx,
                                                );
                                            } else if event.click_count() == 2
                                                && !modifiers.control
                                                && !modifiers.platform
                                                && !modifiers.shift
                                                && !modifiers.function
                                            {
                                                this.start_renaming_tag(
                                                    rename_target.clone(),
                                                    window,
                                                    cx,
                                                );
                                            }
                                            cx.stop_propagation();
                                        },
                                    )),
                            );
                    }

                    cell = cell.child(chip);
                }
            }

            if preview_active {
                cell = cell.child(div());
            } else if Self::editing_is_add(self.editing.as_ref(), &path, key) {
                cell = cell.child(
                    div()
                        .h_flex()
                        .items_center()
                        .w(px(TAG_EDITOR_WIDTH))
                        .flex_shrink_0()
                        .px_1p5()
                        .rounded_md()
                        .text_xs()
                        .bg(cx.theme().muted)
                        .overflow_hidden()
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            if event.keystroke.key == "escape" {
                                this.cancel_tag(window, cx);
                                cx.stop_propagation();
                            }
                        }))
                        .child(
                            Input::new(&self.tag_input)
                                .appearance(false)
                                .xsmall()
                                .px_0()
                                .flex_1()
                                .mr(px(-10.))
                                .min_w_0(),
                        ),
                );
            } else if !convertible {
                let (key, path) = (key.clone(), path.clone());
                cell = cell.child(
                    div()
                        .id(SharedString::from(format!("add:{}:{key}", path.display())))
                        .px_1p5()
                        .rounded_md()
                        .text_xs()
                        .whitespace_nowrap()
                        .text_color(cx.theme().muted_foreground)
                        .cursor_pointer()
                        .opacity(0.)
                        .group_hover(group.clone(), |s| s.opacity(1.))
                        .hover(|s| s.text_color(cx.theme().foreground))
                        .child("+")
                        .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
                        .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_mouse_move(|_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.start_editing(path.clone(), key.clone(), window, cx);
                            cx.stop_propagation();
                        })),
                );
            }

            row = row.child(
                div()
                    .h_full()
                    .relative()
                    .w(*tag_width)
                    .min_w(*tag_width)
                    .flex_shrink_0()
                    .child(cell),
            );
        }

        row = row.child(
            div()
                .h_full()
                .w(layout.tag_key_action_width)
                .min_w(layout.tag_key_action_width)
                .flex_shrink_0(),
        );

        row.into_any_element()
    }
}

impl Render for FileTable {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::perf::sample("table.render.rate");
        let render_start = crate::perf::start();
        let missing_folder_category = {
            let library = self.library.read(cx);
            let active = library.active();
            library.category_needs_folder(active).then_some(active)
        };
        let (keys, tag_widths, row_count) = if missing_folder_category.is_some() {
            (Vec::new(), Vec::new(), 0)
        } else {
            self.table_columns(window, cx)
        };
        let tag_key_action_width = if self.creating_tag_key {
            px(TAG_KEY_EDITOR_WIDTH)
        } else {
            px(TAG_KEY_ACTION_WIDTH)
        };
        let tag_column_keys = self.tag_column_keys(cx);

        let mut header_row = TableRow::new().child(
            TableHead::new().flex_1().min_w_0().px(CONTENT_PX).child(
                div()
                    .h_full()
                    .w_full()
                    .min_w_0()
                    .h_flex()
                    .items_center()
                    .child(div().flex_none().max_w_full().truncate().child("name"))
                    .child(div().h_full().flex_1().min_w_0().on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, event: &MouseDownEvent, window, cx| {
                            this.open_column_visibility_menu(event.position, window, cx);
                            cx.stop_propagation();
                        }),
                    )),
            ),
        );
        for (key, tag_width) in keys.iter().zip(&tag_widths) {
            let key_for_click = key.clone();
            let key_for_menu = key.clone();
            let key_for_hover = key.clone();
            let key_is_delete_hovered = self.alt_down && self.hovered_tag_key.as_ref() == Some(key);
            let table = cx.entity();
            let table_for_label_menu = table.clone();
            let header_label = div()
                .id(SharedString::from(format!("tag-key-header:{key}")))
                .flex_none()
                .max_w_full()
                .truncate()
                .cursor_pointer()
                .text_color(cx.theme().foreground)
                .when(key_is_delete_hovered, |el| el.text_color(red()))
                .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    this.set_tag_key_hovered(key_for_hover.clone(), *hovered, cx);
                }))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        if event.modifiers.alt {
                            this.confirm_tag_key_delete(&key_for_click, window, cx);
                            cx.stop_propagation();
                        }
                    }),
                )
                .context_menu(move |menu, _, _| {
                    let rename_table = table_for_label_menu.clone();
                    let remove_table = table_for_label_menu.clone();
                    let rename_key = key_for_menu.clone();
                    let remove_key = key_for_menu.clone();
                    let remove_label = SharedString::from(format!("Remove {remove_key}"));
                    menu.item(PopupMenuItem::new("Rename").on_click(move |_, window, cx| {
                        let rename_key = rename_key.clone();
                        cx.update_entity(&rename_table, |this, cx| {
                            let target = RenameTarget {
                                kind: RenameKind::TagKey {
                                    old_key: rename_key,
                                },
                            };
                            let initial_value = target.default_value();
                            this.start_rename_target(target, initial_value, window, cx);
                        });
                    }))
                    .separator()
                    .item(
                        PopupMenuItem::new(remove_label).on_click(move |_, window, cx| {
                            cx.update_entity(&remove_table, |this, cx| {
                                this.confirm_tag_key_delete(&remove_key, window, cx);
                            });
                        }),
                    )
                })
                .child(SharedString::from(key.clone()))
                .into_any_element();
            header_row = header_row.child(
                TableHead::new()
                    .w(*tag_width)
                    .min_w(*tag_width)
                    .flex_shrink_0()
                    .pl(CONTENT_PX)
                    .pr(px(0.))
                    .child(
                        div()
                            .h_full()
                            .w_full()
                            .min_w_0()
                            .h_flex()
                            .items_center()
                            .child(header_label)
                            .child(div().h_full().flex_1().min_w_0().on_mouse_down(
                                MouseButton::Right,
                                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                                    this.open_column_visibility_menu(event.position, window, cx);
                                    cx.stop_propagation();
                                }),
                            )),
                    ),
            );
        }
        let creating_tag_key = self.creating_tag_key;
        let tag_key_action = if creating_tag_key {
            div()
                .w_full()
                .h_flex()
                .items_center()
                .rounded_md()
                .bg(cx.theme().muted)
                .text_color(cx.theme().foreground)
                .overflow_hidden()
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        this.cancel_tag_key(window, cx);
                        cx.stop_propagation();
                    }
                }))
                .child(
                    Input::new(&self.tag_key_input)
                        .appearance(false)
                        .xsmall()
                        .text_color(cx.theme().foreground)
                        .px_0()
                        .flex_1()
                        .mr(px(-10.))
                        .min_w_0(),
                )
                .into_any_element()
        } else {
            Button::new("add-tag-key")
                .xsmall()
                .compact()
                .ghost()
                .icon(IconName::Plus)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.start_creating_tag_key(window, cx);
                    cx.stop_propagation();
                }))
                .into_any_element()
        };
        header_row = header_row.child(
            TableHead::new()
                .w(tag_key_action_width)
                .min_w(tag_key_action_width)
                .flex_shrink_0()
                .px(px(4.))
                .child(
                    div()
                        .h_full()
                        .w_full()
                        .min_w_0()
                        .h_flex()
                        .items_center()
                        .child(tag_key_action)
                        .child(div().h_full().flex_1().min_w_0().on_mouse_down(
                            MouseButton::Right,
                            cx.listener(|this, event: &MouseDownEvent, window, cx| {
                                this.open_column_visibility_menu(event.position, window, cx);
                                cx.stop_propagation();
                            }),
                        )),
                ),
        );

        let virtual_keys = keys.clone();
        let virtual_tag_widths = tag_widths.clone();
        let virtual_tag_key_action_width = tag_key_action_width;
        let row_sizes = self.row_sizes(row_count);
        let row_scroll_handle = self.row_scroll_handle.clone();
        let rows: AnyElement = if let Some(category) = missing_folder_category {
            div()
                .id("missing-category-folder")
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .px(CONTENT_PX)
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .cursor_pointer()
                .child(SharedString::from(format!(
                    "Click this area to choose the {} folder.",
                    category.label()
                )))
                .child("You can always change it via the category buttons above.")
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.choose_category_folder(category, cx);
                    cx.stop_propagation();
                }))
                .into_any_element()
        } else {
            div()
                .size_full()
                .child(
                    v_virtual_list(
                        cx.entity().clone(),
                        "file-table-rows",
                        row_sizes,
                        move |this, range, _, cx| {
                            this.render_rows(
                                range,
                                virtual_keys.clone(),
                                virtual_tag_widths.clone(),
                                virtual_tag_key_action_width,
                                cx,
                            )
                        },
                    )
                    .track_scroll(&row_scroll_handle),
                )
                .scrollbar(&row_scroll_handle, ScrollbarAxis::Vertical)
                .into_any_element()
        };
        let column_visibility_menu =
            self.render_column_visibility_menu(tag_column_keys, window, cx);

        let table = div()
            .track_focus(&self.focus_handle)
            .capture_any_mouse_up(cx.listener(|this, event: &MouseUpEvent, window, cx| {
                if event.button == MouseButton::Left {
                    this.finish_local_file_drag(window, cx);
                }
            }))
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, window, cx| {
                    this.finish_local_file_drag(window, cx);
                }),
            )
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "escape"
                        if this.close_column_visibility_menu(cx)
                            || this.cancel_delete(cx)
                            || this.clear_selection(cx) =>
                    {
                        cx.stop_propagation();
                    }
                    "escape" => {}
                    "backspace" | "delete" => {
                        if event.keystroke.modifiers.shift {
                            return;
                        }
                        if this.row_context_menu_open {
                            window.dispatch_keystroke(Keystroke::parse("escape").unwrap(), cx);
                        }
                        if this.confirm_selected_delete(window, cx) {
                            cx.stop_propagation();
                        }
                    }
                    "f2" if this.start_selected_rename(window, cx) => {
                        cx.stop_propagation();
                    }
                    "f2" => {}
                    _ => {}
                }
            }))
            .relative()
            .flex_1()
            .min_h_0()
            .v_flex()
            .child(
                Table::new()
                    .flex_shrink_0()
                    .child(TableHeader::new().child(header_row)),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(div().size_full().child(rows)),
            )
            .when_some(column_visibility_menu, |table, menu| table.child(menu));

        crate::perf::finish("table.render", render_start, || {
            format!(
                "rows={} keys={} editing={}",
                row_count,
                keys.len(),
                self.editing.is_some()
            )
        });
        table
    }
}
