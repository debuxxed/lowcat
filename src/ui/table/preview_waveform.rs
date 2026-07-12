use super::*;

struct PreviewWaveformElement {
    id: ElementId,
    table: Entity<FileTable>,
    path: PathBuf,
    waveform: Option<WaveformBinary256>,
    playhead_bits: Arc<AtomicU32>,
}

pub(super) fn element(
    id: ElementId,
    table: Entity<FileTable>,
    path: PathBuf,
    waveform: Option<WaveformBinary256>,
    playhead_bits: Arc<AtomicU32>,
) -> impl IntoElement {
    PreviewWaveformElement {
        id,
        table,
        path,
        waveform,
        playhead_bits,
    }
}

#[derive(Clone, Copy)]
enum PreviewScrubAction {
    Begin,
    Continue,
    End,
}

impl IntoElement for PreviewWaveformElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for PreviewWaveformElement {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        (
            window.request_layout(
                Style {
                    size: size(relative(1.).into(), relative(1.).into()),
                    ..Style::default()
                },
                [],
                cx,
            ),
            (),
        )
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        _cx: &mut App,
    ) {
        paint_preview_waveform(
            bounds,
            self.waveform,
            FileTable::load_preview_playhead(&self.playhead_bits),
            window,
        );
        let hitbox = prepaint.clone();
        window.set_cursor_style(CursorStyle::PointingHand, &hitbox);

        window.on_mouse_event({
            let table = self.table.clone();
            let path = self.path.clone();
            let hitbox = hitbox.clone();
            move |event: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble
                    || event.button != MouseButton::Left
                    || !event.modifiers.platform
                    || !hitbox.is_hovered(window)
                {
                    return;
                }
                scrub_preview_from_position(
                    &table,
                    &path,
                    event.position,
                    bounds,
                    PreviewScrubAction::Begin,
                    cx,
                );
                cx.stop_propagation();
                window.prevent_default();
            }
        });

        window.on_mouse_event({
            let table = self.table.clone();
            let path = self.path.clone();
            move |event: &MouseMoveEvent, phase, _, cx| {
                if phase != DispatchPhase::Bubble || !event.dragging() || !event.modifiers.platform
                {
                    return;
                }
                scrub_preview_from_position(
                    &table,
                    &path,
                    event.position,
                    bounds,
                    PreviewScrubAction::Continue,
                    cx,
                );
                cx.stop_propagation();
            }
        });

        window.on_mouse_event({
            let table = self.table.clone();
            let path = self.path.clone();
            move |event: &MouseUpEvent, phase, _, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                scrub_preview_from_position(
                    &table,
                    &path,
                    event.position,
                    bounds,
                    PreviewScrubAction::End,
                    cx,
                );
            }
        });
    }
}

fn paint_preview_waveform(
    bounds: Bounds<Pixels>,
    waveform: Option<WaveformBinary256>,
    playhead_ratio: Option<f32>,
    window: &mut Window,
) {
    let row_width = bounds.size.width.as_f32().max(1.);
    let color = white();

    if let Some(waveform) = waveform {
        let row_height = bounds.size.height.as_f32().max(1.);
        let bar_gap = 1.;
        let bar_width = ((row_width - bar_gap * (WAVEFORM_BAR_COUNT - 1) as f32)
            / WAVEFORM_BAR_COUNT as f32)
            .max(1.);

        for (ix, value) in waveform.into_iter().enumerate() {
            let height = if value == 0 {
                1.
            } else {
                ((value as f32 / 255.) * row_height).max(1.)
            };
            let x = bounds.left().as_f32() + ix as f32 * (bar_width + bar_gap);
            let y = bounds.bottom().as_f32() - height;
            window.paint_quad(fill(
                Bounds::new(point(px(x), px(y)), size(px(bar_width), px(height))),
                color,
            ));
        }
    }

    if let Some(ratio) = playhead_ratio {
        let x = bounds.left().as_f32() + row_width * ratio.clamp(0., 1.);
        window.paint_quad(fill(
            Bounds::new(point(px(x), bounds.top()), size(px(2.), bounds.size.height)),
            color,
        ));
    }
}

fn scrub_preview_from_position(
    table: &Entity<FileTable>,
    path: &Path,
    position: Point<Pixels>,
    bounds: Bounds<Pixels>,
    action: PreviewScrubAction,
    cx: &mut App,
) {
    let ratio = ((position.x.as_f32() - bounds.left().as_f32())
        / bounds.size.width.as_f32().max(1.))
    .clamp(0., 1.);
    cx.update_entity(table, |table, cx| match action {
        PreviewScrubAction::Begin => {
            table.begin_preview_scrub(path.to_path_buf(), ratio, cx);
        }
        PreviewScrubAction::Continue => {
            table.continue_preview_scrub(path, ratio, cx);
        }
        PreviewScrubAction::End => {
            table.end_preview_scrub(path, cx);
        }
    });
}
