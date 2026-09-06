use gpui::{
    AnyElement, App, Bounds, BoxShadow, ClickEvent, CursorStyle, Decorations, HitboxBehavior, Hsla,
    IntoElement, MouseButton, Pixels, Point, ResizeEdge, Size, Tiling, Window, WindowButton,
    WindowButtonLayout, WindowControls, canvas, px, size, transparent_black,
};
use theme::ActiveTheme as _;
use ui::prelude::*;
use ui::{IconButton, IconButtonShape, IconName, IconSize};

pub(crate) const TITLE_BAR_HEIGHT: Pixels = px(28.);

pub(crate) fn render_window_controls<F>(
    window: &Window,
    cx: &mut App,
    on_close: F,
) -> (AnyElement, AnyElement)
where
    F: Fn(&ClickEvent, &mut Window, &mut App) + 'static,
{
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        if !matches!(window.window_decorations(), Decorations::Client { .. }) {
            return (div().into_any_element(), div().into_any_element());
        }

        let controls = window.window_controls();
        let layout = complete_button_layout(cx);
        let mut close_handler = Some(on_close);
        let left = render_button_side(
            layout.left,
            window.is_maximized(),
            controls,
            &mut close_handler,
        );
        let right = render_button_side(
            layout.right,
            window.is_maximized(),
            controls,
            &mut close_handler,
        );
        return (left, right);
    }

    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    {
        let _ = (window, cx, on_close);
        (div().into_any_element(), div().into_any_element())
    }
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn complete_button_layout(cx: &mut App) -> WindowButtonLayout {
    let mut layout = cx
        .button_layout()
        .unwrap_or_else(WindowButtonLayout::linux_default);
    for button in [
        WindowButton::Minimize,
        WindowButton::Maximize,
        WindowButton::Close,
    ] {
        let is_present = layout
            .left
            .iter()
            .chain(layout.right.iter())
            .any(|candidate| *candidate == Some(button));
        if !is_present {
            if let Some(slot) = layout.right.iter_mut().find(|slot| slot.is_none()) {
                *slot = Some(button);
            }
        }
    }
    layout
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn render_button_side<F>(
    buttons: [Option<WindowButton>; 3],
    is_maximized: bool,
    controls: WindowControls,
    close_handler: &mut Option<F>,
) -> AnyElement
where
    F: Fn(&ClickEvent, &mut Window, &mut App) + 'static,
{
    let buttons = buttons
        .into_iter()
        .flatten()
        .filter(|button| match button {
            WindowButton::Minimize => controls.minimize,
            WindowButton::Maximize => controls.maximize,
            WindowButton::Close => true,
        })
        .map(|button| {
            let close_handler = if matches!(button, WindowButton::Close) {
                close_handler.take()
            } else {
                None
            };
            render_window_button(button, is_maximized, close_handler)
        });

    h_flex()
        .h(TITLE_BAR_HEIGHT - px(1.))
        .flex_none()
        .items_center()
        .children(buttons)
        .into_any_element()
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn render_window_button<F>(
    button: WindowButton,
    is_maximized: bool,
    close_handler: Option<F>,
) -> AnyElement
where
    F: Fn(&ClickEvent, &mut Window, &mut App) + 'static,
{
    let (icon, label) = match button {
        WindowButton::Minimize => (IconName::GenericMinimize, "Minimize"),
        WindowButton::Maximize if is_maximized => (IconName::GenericRestore, "Restore"),
        WindowButton::Maximize => (IconName::GenericMaximize, "Maximize"),
        WindowButton::Close => (IconName::GenericClose, "Close"),
    };

    let button_id = button.id();
    let button = match button {
        WindowButton::Minimize => IconButton::new(button.id(), icon)
            .icon_size(IconSize::XSmall)
            .shape(IconButtonShape::Square)
            .style(ButtonStyle::Subtle)
            .aria_label(label)
            .on_click(|_, window, cx| {
                cx.stop_propagation();
                window.minimize_window();
            })
            .into_any_element(),
        WindowButton::Maximize => IconButton::new(button.id(), icon)
            .icon_size(IconSize::XSmall)
            .shape(IconButtonShape::Square)
            .style(ButtonStyle::Subtle)
            .aria_label(label)
            .on_click(|_, window, cx| {
                cx.stop_propagation();
                window.zoom_window();
            })
            .into_any_element(),
        WindowButton::Close => {
            let Some(close_handler) = close_handler else {
                return div().into_any_element();
            };
            IconButton::new(button.id(), icon)
                .icon_size(IconSize::XSmall)
                .shape(IconButtonShape::Square)
                .style(ButtonStyle::Subtle)
                .aria_label(label)
                .on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    close_handler(event, window, cx);
                })
                .into_any_element()
        }
    };

    div()
        .id(format!("window-control-{button_id}"))
        .h(TITLE_BAR_HEIGHT - px(1.))
        .flex()
        .items_center()
        .px_1()
        .on_mouse_down(MouseButton::Left, |_, _, cx| {
            cx.stop_propagation();
        })
        .child(button)
        .into_any_element()
}

pub(crate) fn client_side_decorations(
    element: impl IntoElement,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let decorations = window.window_decorations();
    let is_client = matches!(decorations, Decorations::Client { .. });
    let tiling = match decorations {
        Decorations::Server => Tiling::default(),
        Decorations::Client { tiling } => tiling,
    };
    let shadow_size = theme::CLIENT_SIDE_DECORATION_SHADOW;
    let is_resizable = window.is_resizable();

    window.set_client_inset(if is_client { shadow_size } else { px(0.) });

    let mut backdrop = div()
        .id("window-backdrop")
        .size_full()
        .bg(transparent_black());
    if is_client {
        backdrop = backdrop
            .when(!tiling.top, |this| this.pt(shadow_size))
            .when(!tiling.bottom, |this| this.pb(shadow_size))
            .when(!tiling.left, |this| this.pl(shadow_size))
            .when(!tiling.right, |this| this.pr(shadow_size));

        if is_resizable {
            backdrop = backdrop
                .child(
                    canvas(
                        |_bounds, window, _cx| {
                            window.insert_hitbox(
                                Bounds::new(
                                    Point::default(),
                                    window.window_bounds().get_bounds().size,
                                ),
                                HitboxBehavior::Normal,
                            )
                        },
                        move |_bounds, hitbox, window, _cx| {
                            let size = window.window_bounds().get_bounds().size;
                            if let Some(edge) =
                                resize_edge(window.mouse_position(), shadow_size, size, tiling)
                            {
                                window
                                    .set_cursor_style(cursor_style_for_resize_edge(edge), &hitbox);
                            }
                        },
                    )
                    .size_full()
                    .absolute(),
                )
                .on_mouse_move(|_, window, _| window.refresh())
                .on_mouse_down(MouseButton::Left, move |event, window, _| {
                    let size = window.window_bounds().get_bounds().size;
                    if let Some(edge) = resize_edge(event.position, shadow_size, size, tiling) {
                        window.start_window_resize(edge);
                    }
                });
        }
    }

    let mut content = div().id("window-content").size_full().child(element);
    if is_client {
        let border_color = cx.theme().colors().border;
        content = content
            .border_color(border_color)
            .when(!tiling.top, |this| this.border_t_1())
            .when(!tiling.bottom, |this| this.border_b_1())
            .when(!tiling.left, |this| this.border_l_1())
            .when(!tiling.right, |this| this.border_r_1())
            .when(
                !tiling.top || !tiling.bottom || !tiling.left || !tiling.right,
                |this| {
                    this.shadow(vec![
                        BoxShadow::new(
                            px(0.),
                            px(0.),
                            Hsla {
                                h: 0.,
                                s: 0.,
                                l: 0.,
                                a: 0.4,
                            },
                        )
                        .blur_radius(shadow_size / 2.),
                    ])
                },
            );
    }

    backdrop.child(content)
}

fn cursor_style_for_resize_edge(edge: ResizeEdge) -> CursorStyle {
    match edge {
        ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
        ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ResizeLeftRight,
        ResizeEdge::TopLeft | ResizeEdge::BottomRight => CursorStyle::ResizeUpLeftDownRight,
        ResizeEdge::TopRight | ResizeEdge::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
    }
}

fn resize_edge(
    position: Point<gpui::Pixels>,
    shadow_size: gpui::Pixels,
    window_size: Size<gpui::Pixels>,
    tiling: Tiling,
) -> Option<ResizeEdge> {
    let inner_bounds = Bounds::new(Point::default(), window_size).inset(shadow_size * 1.5);
    if inner_bounds.contains(&position) {
        return None;
    }

    let corner_size = size(shadow_size * 1.5, shadow_size * 1.5);
    let top_left_bounds = Bounds::new(Point::new(px(0.), px(0.)), corner_size);
    if !tiling.top && top_left_bounds.contains(&position) {
        return Some(ResizeEdge::TopLeft);
    }

    let top_right_bounds = Bounds::new(
        Point::new(window_size.width - corner_size.width, px(0.)),
        corner_size,
    );
    if !tiling.top && top_right_bounds.contains(&position) {
        return Some(ResizeEdge::TopRight);
    }

    let bottom_left_bounds = Bounds::new(
        Point::new(px(0.), window_size.height - corner_size.height),
        corner_size,
    );
    if !tiling.bottom && bottom_left_bounds.contains(&position) {
        return Some(ResizeEdge::BottomLeft);
    }

    let bottom_right_bounds = Bounds::new(
        Point::new(
            window_size.width - corner_size.width,
            window_size.height - corner_size.height,
        ),
        corner_size,
    );
    if !tiling.bottom && bottom_right_bounds.contains(&position) {
        return Some(ResizeEdge::BottomRight);
    }

    if !tiling.top && position.y < inner_bounds.origin.y {
        return Some(ResizeEdge::Top);
    }
    if !tiling.bottom && position.y > inner_bounds.bottom() {
        return Some(ResizeEdge::Bottom);
    }
    if !tiling.left && position.x < inner_bounds.origin.x {
        return Some(ResizeEdge::Left);
    }
    if !tiling.right && position.x > inner_bounds.right() {
        return Some(ResizeEdge::Right);
    }
    None
}
