//! Animaciones de la interfaz.
//!
//! Reglas (docs/diseno/temas.md §5): entradas con curva out-expo, sin
//! desplazamientos, nada rebota y nada
//! pulsa en bucle salvo el progreso de un agente trabajando, que va a velocidad
//! constante. `with_animation` ya respeta "Reducir movimiento" del sistema:
//! con esa opción activa todo se pinta en su estado final.
//!
//! Carga: lo que se repite (spinner, brillo, parpadeo, progreso) va con
//! `with_max_fps`, así la ventana se repinta 8-15 veces por segundo en vez de
//! 60 mientras un agente trabaja. Lo que pasa una vez dura menos de 320 ms.

use gpui_kit::base::animation::cubic_bezier;
use gpui_kit::{
    Animation, AnimationElement, AnimationExt as _, App, Div, ElementId, Hsla, IntoElement, ParentElement, Pixels, ScrollEvent,
    ScrollHandle, Styled, Window, div, point, px,
};
use std::time::{Duration, Instant};

pub const FAST: Duration = Duration::from_millis(160);
pub const NORMAL: Duration = Duration::from_millis(240);
pub const SLOW: Duration = Duration::from_millis(320);
/// Desfase entre filas de una lista.
pub const STAGGER: Duration = Duration::from_millis(50);
/// A partir de esta fila ya no se espera más: entran todas juntas.
pub const MAX_STAGGERED: usize = 8;

/// Entrada: arranca rápido y aterriza con cola larga (out-expo).
pub fn enter() -> impl Fn(f32) -> f32 {
    cubic_bezier(0.16, 1.0, 0.3, 1.0)
}

/// Una sola vez, con la curva de entrada.
pub fn once(duration: Duration) -> Animation {
    Animation::new(duration).with_easing(enter())
}

/// Aparece con un fundido, sin desplazarse.
pub fn fade_in<E>(el: E, id: impl Into<ElementId>, duration: Duration) -> AnimationElement<E>
where
    E: IntoElement + Styled + 'static,
{
    el.with_animation(id, once(duration), |el, t| el.opacity(t))
}

/// La fila `index` de una lista: pasa de tenue a su color normal después de
/// esperar su turno. No se mueve.
pub fn row_in<E>(el: E, id: impl Into<ElementId>, index: usize) -> AnimationElement<E>
where
    E: IntoElement + Styled + 'static,
{
    let delay = STAGGER * index.min(MAX_STAGGERED) as u32;
    let total = delay + NORMAL;
    let ease = enter();
    let start = delay.as_secs_f32() / total.as_secs_f32();
    el.with_animation(id, Animation::new(total), move |el, t| {
        let local = ((t - start) / (1. - start)).clamp(0., 1.);
        el.opacity(0.25 + 0.75 * ease(local))
    })
}

/// Cuadros por segundo de lo que se repite.
pub const LOOP_FPS: f32 = 8.;
/// Brillo del verbo y barra de progreso: un poco más fluidos.
pub const SMOOTH_FPS: f32 = 15.;

/// Glifos del spinner, como el de Claude Code: ida y vuelta. Sin `✳`: Windows
/// lo pinta con la fuente de emoji (un cuadro verde); `✱` no es emoji.
const SPINNER: [&str; 10] = ["·", "✢", "✱", "✶", "✻", "✽", "✻", "✶", "✱", "✢"];

/// Spinner de "trabajando": un glifo que cambia 8 veces por segundo.
pub fn spinner(id: impl Into<ElementId>, color: impl Into<Hsla>, size: Pixels) -> AnimationElement<Div> {
    let frame = 1. / LOOP_FPS;
    let anim = Animation::new(Duration::from_secs_f32(frame * SPINNER.len() as f32)).repeat_synced().with_max_fps(LOOP_FPS);
    div()
        .w(size)
        .flex_none()
        .flex()
        .justify_center()
        .text_size(size)
        .line_height(size)
        .text_color(color.into())
        .with_animation(id, anim, |d, t| d.child(SPINNER[((t * SPINNER.len() as f32) as usize).min(SPINNER.len() - 1)]))
}

/// Scroll vertical que se desliza con la rueda del ratón: en Windows cada
/// clic salta 3 líneas de golpe y se ve a brincos; así llega en ~150 ms. El
/// trackpad (píxeles) y "Reducir movimiento" se aplican tal cual.
/// Uso: `.overflow_y_hidden().track_scroll(&s.handle)` + `on_scroll` → [`Self::wheel`],
/// y [`Self::tick`] al principio de `render`.
pub struct SmoothScroll {
    pub handle: ScrollHandle,
    target: Option<Pixels>,
    last: Option<Instant>,
}

impl SmoothScroll {
    pub fn new() -> Self {
        Self { handle: ScrollHandle::new(), target: None, last: None }
    }

    pub fn wheel(&mut self, ev: &ScrollEvent, window: &Window, cx: &App) {
        let max = self.handle.max_offset().y;
        let dy = ev.delta.pixel_delta(window.line_height()).y;
        if ev.delta.precise() || cx.reduce_motion() {
            self.target = None;
            self.handle.set_offset(point(px(0.), (self.handle.offset().y + dy).clamp(-max, px(0.))));
        } else {
            // Clics seguidos suman al destino pendiente, no al punto a medio camino.
            let from = self.target.unwrap_or_else(|| self.handle.offset().y);
            self.target = Some((from + dy).clamp(-max, px(0.)));
            self.last = None;
        }
    }

    /// Un paso hacia el destino; pide otro cuadro mientras no llegue.
    pub fn tick(&mut self, window: &Window) {
        let Some(target) = self.target else { return };
        let now = Instant::now();
        let dt = self.last.map_or(1. / 60., |t| now.duration_since(t).as_secs_f32().min(0.1));
        self.last = Some(now);
        let y = self.handle.offset().y;
        let next = y + (target - y) * (1. - (-dt / 0.045).exp());
        if (target - next).abs() < px(0.5) {
            self.handle.set_offset(point(px(0.), target));
            self.target = None;
        } else {
            self.handle.set_offset(point(px(0.), next));
            window.request_animation_frame();
        }
    }
}

/// Parpadeo lento (un ciclo por segundo) para lo que está en curso.
pub fn blink<E>(el: E, id: impl Into<ElementId>) -> AnimationElement<E>
where
    E: IntoElement + Styled + 'static,
{
    let anim = Animation::new(Duration::from_millis(1000)).repeat_synced().with_max_fps(LOOP_FPS);
    el.with_animation(id, anim, |el, t| el.opacity(if t < 0.5 { 1. } else { 0.35 }))
}

/// Texto con un brillo que lo recorre de izquierda a derecha, como el verbo
/// de Claude Code mientras trabaja. Para textos cortos (una palabra o dos).
pub fn shimmer(id: impl Into<ElementId>, text: String, base: Hsla, bright: Hsla) -> AnimationElement<Div> {
    let chars: Vec<String> = text.chars().map(String::from).collect();
    let n = chars.len().max(1) as f32;
    let anim = Animation::new(Duration::from_millis(1800)).repeat_synced().with_max_fps(SMOOTH_FPS);
    div().flex().with_animation(id, anim, move |d, t| {
        // El brillo entra por la izquierda y sale por la derecha (3 letras de ancho).
        let center = -3. + t * (n + 6.);
        d.children(chars.iter().enumerate().map(|(i, ch)| {
            let glow = (1. - ((i as f32 - center).abs() / 3.)).clamp(0., 1.);
            div().text_color(mix(base, bright, glow)).child(ch.clone())
        }))
    })
}

fn mix(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let (a, b) = (gpui_kit::Rgba::from(a), gpui_kit::Rgba::from(b));
    gpui_kit::Rgba {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a + (b.a - a.a) * t,
    }
    .into()
}

/// Valor intermedio entre `from` y `to` para el avance `t` (0..1).
pub fn lerp(from: f64, to: f64, t: f32) -> f64 {
    from + (to - from) * t as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_curve_starts_and_ends_in_place() {
        let f = enter();
        assert!(f(0.).abs() < 1e-3);
        assert!((f(1.) - 1.).abs() < 1e-3);
        // La entrada va adelantada: a mitad de tiempo ya recorrió casi todo.
        assert!(enter()(0.5) > 0.85);
        assert_eq!(lerp(10., 20., 0.5), 15.);
    }
}
