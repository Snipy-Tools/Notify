//! Geometrie von Pille, Panel und Einstellungs-Popover. Alles in physischen Pixeln und ohne Fenster,
//! damit sich die Regeln testen lassen.

use crate::settings::Aufklappen;

/// Logische Grössen (vor der Skalierung des Monitors)
pub const PILL_W: f64 = 400.0;
pub const PILL_H: f64 = 64.0;
pub const PANEL_W: f64 = 400.0;
pub const PANEL_H: f64 = 560.0;
/// Abstand zwischen Pille und Panel, und zwischen Leiste und Popover
pub const GAP: f64 = 24.0;
/// Abstand zum Bildschirmrand, wenn die Leiste einrastet
pub const MARGIN: f64 = 12.0;
/// So nah am Rand rastet die Leiste ein
pub const SNAP: f64 = 24.0;

/// Rechteck in Pixeln
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub const fn right(&self) -> i32 {
        self.x + self.w
    }

    pub const fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub const fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    pub fn intersects(&self, other: &Rect) -> bool {
        self.x < other.right() && other.x < self.right() && self.y < other.bottom() && other.y < self.bottom()
    }

    fn inside(&self, work: &Rect) -> bool {
        self.x >= work.x && self.y >= work.y && self.right() <= work.right() && self.bottom() <= work.bottom()
    }
}

/// Wohin das Panel tatsächlich aufklappt
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// Pille oben, Panel darunter
    Down,
    /// Panel oben, Pille darunter
    Up,
}

/// Richtung für die Pille an ihrer jetzigen Position. `work` ist der Arbeitsbereich ihres Monitors.
/// Automatisch: ist unter der Pille mehr Platz bis zum Rand als darüber, geht es nach unten, sonst nach oben.
pub fn resolve_dir(pref: Aufklappen, pill: Rect, work: Rect) -> Dir {
    match pref {
        Aufklappen::Unten => Dir::Down,
        Aufklappen::Oben => Dir::Up,
        Aufklappen::Auto => {
            let below = work.bottom() - pill.bottom();
            let above = pill.y - work.y;
            if below > above { Dir::Down } else { Dir::Up }
        }
    }
}

/// Wo das Panel steht, wenn die Pille bei `pill` steht (mittig zur Pille, mit `gap` Abstand)
pub fn panel_origin(pill: (i32, i32), pill_size: (i32, i32), panel_size: (i32, i32), gap: i32, dir: Dir) -> (i32, i32) {
    let x = pill.0 + (pill_size.0 - panel_size.0) / 2;
    let y = match dir {
        Dir::Down => pill.1 + pill_size.1 + gap,
        Dir::Up => pill.1 - gap - panel_size.1,
    };
    (x, y)
}

/// Pille und (falls aufgeklappt) Panel zusammen
pub fn group_rect(pill: (i32, i32), pill_size: (i32, i32), panel_size: (i32, i32), gap: i32, dir: Option<Dir>) -> Rect {
    let pill_rect = Rect::new(pill.0, pill.1, pill_size.0, pill_size.1);
    let Some(dir) = dir else { return pill_rect };
    let (px, py) = panel_origin(pill, pill_size, panel_size, gap, dir);
    let x = pill_rect.x.min(px);
    let y = pill_rect.y.min(py);
    let right = pill_rect.right().max(px + panel_size.0);
    let bottom = pill_rect.bottom().max(py + panel_size.1);
    Rect::new(x, y, right - x, bottom - y)
}

/// Ergebnis des Aufklappens
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub pill: (i32, i32),
    pub panel: (i32, i32),
    pub dir: Dir,
}

impl Placement {
    /// Wurde die Pille verschoben, weil das Panel sonst nicht auf den Monitor passt?
    pub fn pill_moved(&self, from: (i32, i32)) -> bool {
        self.pill != from
    }
}

/// Klappt das Panel neben der Pille auf. Die Pille bleibt stehen, ausser das Panel passt sonst nicht
/// auf den Arbeitsbereich: dann wird die Pille gerade so weit verschoben, dass alles passt.
pub fn expand(
    pref: Aufklappen,
    pill: (i32, i32),
    pill_size: (i32, i32),
    panel_size: (i32, i32),
    gap: i32,
    work: Rect,
) -> Placement {
    let pill_rect = Rect::new(pill.0, pill.1, pill_size.0, pill_size.1);
    let dir = resolve_dir(pref, pill_rect, work);

    let group = group_rect(pill, pill_size, panel_size, gap, Some(dir));
    let mut dx = 0;
    let mut dy = 0;
    // zuerst unten/rechts begrenzen, dann oben/links (ist die Gruppe grösser als der Monitor, gewinnt oben links)
    if group.bottom() > work.bottom() {
        dy = work.bottom() - group.bottom();
    }
    if group.y + dy < work.y {
        dy = work.y - group.y;
    }
    if group.right() > work.right() {
        dx = work.right() - group.right();
    }
    if group.x + dx < work.x {
        dx = work.x - group.x;
    }
    let pill = (pill.0 + dx, pill.1 + dy);
    Placement { pill, panel: panel_origin(pill, pill_size, panel_size, gap, dir), dir }
}

/// Hält die Gruppe (`size`) auf dem Arbeitsbereich; mit `snap` rastet sie nahe am Rand ein.
/// `scale` skaliert Abstand und Reichweite des Einrastens.
pub fn clamp_and_snap(pos: (i32, i32), size: (i32, i32), work: Rect, scale: f64, snap: bool) -> (i32, i32) {
    let (w, h) = size;
    let mut x = pos.0.clamp(work.x, (work.right() - w).max(work.x));
    let mut y = pos.1.clamp(work.y, (work.bottom() - h).max(work.y));

    if snap {
        let near = (SNAP * scale).round() as i32;
        let margin = (MARGIN * scale).round() as i32;
        if x - work.x < near {
            x = work.x + margin;
        } else if work.right() - (x + w) < near {
            x = work.right() - w - margin;
        }
        if y - work.y < near {
            y = work.y + margin;
        } else if work.bottom() - (y + h) < near {
            y = work.bottom() - h - margin;
        }
        // das Einrasten darf die Gruppe nicht über den Rand schieben (kleiner Monitor)
        x = x.clamp(work.x, (work.right() - w).max(work.x));
        y = y.clamp(work.y, (work.bottom() - h).max(work.y));
    }
    (x, y)
}

/// Verschiebt die Pille so, dass die ganze Gruppe auf dem Arbeitsbereich liegt (mit `snap` auch einrastend)
pub fn settle_pill(
    pill: (i32, i32),
    pill_size: (i32, i32),
    panel_size: (i32, i32),
    gap: i32,
    dir: Option<Dir>,
    work: Rect,
    scale: f64,
    snap: bool,
) -> (i32, i32) {
    let group = group_rect(pill, pill_size, panel_size, gap, dir);
    let (x, y) = clamp_and_snap((group.x, group.y), (group.w, group.h), work, scale, snap);
    (pill.0 + x - group.x, pill.1 + y - group.y)
}

/// Wo das Popover (Einstellungen) steht: vollständig auf dem Arbeitsbereich und ohne die Leiste (`bar`,
/// Pille und Panel) zu verdecken. Bevorzugt rechts, dann links neben der Leiste, sonst unter oder über ihr.
/// Passt nichts, wird das Popover wenigstens auf den Bildschirm geschoben.
pub fn anchor_popover(bar: Rect, size: (i32, i32), work: Rect, gap: i32) -> (i32, i32) {
    let (w, h) = size;
    let fit_x = |x: i32| x.clamp(work.x, (work.right() - w).max(work.x));
    let fit_y = |y: i32| y.clamp(work.y, (work.bottom() - h).max(work.y));
    // seitlich: senkrecht zur Leiste zentriert
    let side_y = fit_y(bar.y + (bar.h - h) / 2);
    // oben/unten: waagrecht zur Leiste zentriert
    let flat_x = fit_x(bar.x + (bar.w - w) / 2);

    let candidates = [
        (bar.right() + gap, side_y),
        (bar.x - gap - w, side_y),
        (flat_x, bar.bottom() + gap),
        (flat_x, bar.y - gap - h),
    ];
    for (x, y) in candidates {
        let rect = Rect::new(x, y, w, h);
        if rect.inside(&work) && !rect.intersects(&bar) {
            return (x, y);
        }
    }
    // Nichts passt ohne Überlappung: die Seite mit dem meisten freien Platz, auf den Bildschirm geschoben
    let space = [
        (work.right() - bar.right(), 0),
        (bar.x - work.x, 1),
        (work.bottom() - bar.bottom(), 2),
        (bar.y - work.y, 3),
    ];
    let best = space.iter().max_by_key(|(s, _)| *s).map_or(0, |(_, i)| *i);
    let (x, y) = candidates[best];
    (fit_x(x), fit_y(y))
}

/// Wo das Hinweisfenster steht: unten rechts im Arbeitsbereich. Verdeckt das die Leiste (`bar`), steht es daneben.
pub fn reminder_position(work: Rect, size: (i32, i32), margin: i32, bar: Option<Rect>, gap: i32) -> (i32, i32) {
    let corner = (work.right() - size.0 - margin, work.bottom() - size.1 - margin);
    let rect = Rect::new(corner.0, corner.1, size.0, size.1);
    match bar {
        Some(bar) if rect.intersects(&bar) => anchor_popover(bar, size, work, gap),
        _ => corner,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK: Rect = Rect::new(0, 0, 1920, 1152);
    const PILL: (i32, i32) = (400, 64);
    const PANEL: (i32, i32) = (400, 560);
    const G: i32 = 24;

    fn exp(pref: Aufklappen, pill: (i32, i32), work: Rect) -> Placement {
        expand(pref, pill, PILL, PANEL, G, work)
    }

    #[test]
    fn auto_goes_where_there_is_more_room() {
        let at = |y| Rect::new(500, y, 400, 64);
        // oben auf dem Bildschirm: mehr Platz darunter
        assert_eq!(resolve_dir(Aufklappen::Auto, at(10), WORK), Dir::Down);
        // unten: mehr Platz darüber
        assert_eq!(resolve_dir(Aufklappen::Auto, at(1000), WORK), Dir::Up);
        // knapp unter der Mitte: 1152 - (580 + 64) = 508 unten, 580 oben
        assert_eq!(resolve_dir(Aufklappen::Auto, at(580), WORK), Dir::Up);
        assert_eq!(resolve_dir(Aufklappen::Auto, at(500), WORK), Dir::Down);
        // Gleichstand: nach oben
        let even = Rect::new(0, (1152 - 64) / 2, 400, 64);
        assert_eq!(WORK.bottom() - even.bottom(), even.y - WORK.y);
        assert_eq!(resolve_dir(Aufklappen::Auto, even, WORK), Dir::Up);
        // feste Richtung gewinnt
        assert_eq!(resolve_dir(Aufklappen::Unten, at(1000), WORK), Dir::Down);
        assert_eq!(resolve_dir(Aufklappen::Oben, at(10), WORK), Dir::Up);
    }

    #[test]
    fn auto_uses_the_work_area_of_the_pills_monitor() {
        // zweiter Monitor links, niedriger und tiefer gesetzt
        let left = Rect::new(-1280, 200, 1280, 720);
        let pill = Rect::new(-900, 230, 400, 64);
        assert_eq!(resolve_dir(Aufklappen::Auto, pill, left), Dir::Down);
        let low = Rect::new(-900, 800, 400, 64);
        assert_eq!(resolve_dir(Aufklappen::Auto, low, left), Dir::Up);
    }

    #[test]
    fn expanding_down_keeps_the_pill_where_it_is() {
        let p = exp(Aufklappen::Auto, (500, 100), WORK);
        assert_eq!(p.dir, Dir::Down);
        assert_eq!(p.pill, (500, 100));
        assert!(!p.pill_moved((500, 100)));
        // Panel genau 24 px unter der Pille
        assert_eq!(p.panel, (500, 100 + 64 + 24));
    }

    #[test]
    fn expanding_up_puts_the_panel_above_with_the_gap() {
        let p = exp(Aufklappen::Auto, (500, 1000), WORK);
        assert_eq!(p.dir, Dir::Up);
        assert_eq!(p.pill, (500, 1000));
        assert_eq!(p.panel, (500, 1000 - 24 - 560));
        // Abstand zwischen Panel-Unterkante und Pille: 24
        assert_eq!(p.pill.1 - (p.panel.1 + 560), 24);
    }

    #[test]
    fn expanding_with_exactly_enough_room_does_not_move() {
        // Down: Pille so, dass das Panel genau am unteren Rand endet
        let y = WORK.bottom() - 560 - 24 - 64;
        let p = exp(Aufklappen::Unten, (500, y), WORK);
        assert_eq!((p.pill, p.panel.1 + 560), ((500, y), WORK.bottom()));
        // einen Pixel tiefer: die Pille muss um einen Pixel hoch
        let p = exp(Aufklappen::Unten, (500, y + 1), WORK);
        assert_eq!((p.pill.1, p.panel.1 + 560), (y, WORK.bottom()));
        // Up: Panel beginnt genau am oberen Rand
        let p = exp(Aufklappen::Oben, (500, 560 + 24), WORK);
        assert_eq!((p.pill, p.panel.1), ((500, 584), 0));
    }

    #[test]
    fn pill_moves_only_when_the_panel_does_not_fit() {
        // erzwungen nach unten, Pille ganz unten: sie rückt hoch, das Panel liegt bündig am Rand
        let p = exp(Aufklappen::Unten, (500, 1080), WORK);
        assert_eq!(p.dir, Dir::Down);
        assert_eq!(p.panel.1 + 560, WORK.bottom());
        assert_eq!(p.pill.1, WORK.bottom() - 560 - 24 - 64);
        assert_eq!(p.panel.1 - (p.pill.1 + 64), 24);
        assert!(p.pill_moved((500, 1080)));
        // erzwungen nach oben, Pille ganz oben: sie rückt runter
        let p = exp(Aufklappen::Oben, (500, 0), WORK);
        assert_eq!(p.dir, Dir::Up);
        assert_eq!(p.panel.1, 0);
        assert_eq!(p.pill.1, 560 + 24);
    }

    #[test]
    fn auto_on_a_small_monitor_picks_the_bigger_side_and_shifts() {
        let small = Rect::new(0, 0, 1366, 700);
        let p = exp(Aufklappen::Auto, (100, 300), small);
        // unten sind 700 - 364 = 336 frei, oben 300: nach unten
        assert_eq!(p.dir, Dir::Down);
        assert_eq!(p.panel.1 + 560, small.bottom());
        assert_eq!(p.pill.1, 700 - 560 - 24 - 64);
        // alles liegt auf dem Monitor
        let g = group_rect(p.pill, PILL, PANEL, G, Some(p.dir));
        assert!(g.inside(&small));
    }

    #[test]
    fn group_taller_than_the_monitor_stays_top_aligned() {
        let tiny = Rect::new(0, 0, 800, 500);
        let p = exp(Aufklappen::Unten, (100, 200), tiny);
        assert_eq!(p.pill.1, 0);
        let p = exp(Aufklappen::Oben, (100, 200), tiny);
        assert_eq!(p.panel.1, 0);
    }

    #[test]
    fn horizontal_overflow_shifts_the_group_left_or_right() {
        // rechts hinausragend
        let p = exp(Aufklappen::Unten, (1700, 100), WORK);
        assert_eq!(p.pill, (1520, 100));
        assert_eq!(p.panel.0, 1520);
        // links hinausragend
        let p = exp(Aufklappen::Unten, (-50, 100), WORK);
        assert_eq!(p.pill, (0, 100));
        // Monitor mit negativem Ursprung
        let left = Rect::new(-1920, 0, 1920, 1080);
        let p = exp(Aufklappen::Unten, (-300, 100), left);
        assert_eq!(p.pill, (-400, 100));
    }

    #[test]
    fn panel_is_centered_on_a_pill_of_other_width() {
        let o = panel_origin((100, 100), (300, 64), (400, 560), 24, Dir::Down);
        assert_eq!(o, (50, 188));
    }

    #[test]
    fn scaled_sizes_scale_the_gap_too() {
        // Skalierung 1.5: 600x96 Pille, 600x840 Panel, 36 px Abstand
        let work = Rect::new(0, 0, 2880, 1728);
        let p = expand(Aufklappen::Unten, (100, 100), (600, 96), (600, 840), 36, work);
        assert_eq!(p.panel, (100, 100 + 96 + 36));
        assert_eq!(p.pill, (100, 100));
    }

    #[test]
    fn group_rect_covers_both_windows() {
        let down = group_rect((100, 100), PILL, PANEL, G, Some(Dir::Down));
        assert_eq!(down, Rect::new(100, 100, 400, 64 + 24 + 560));
        let up = group_rect((100, 800), PILL, PANEL, G, Some(Dir::Up));
        assert_eq!(up, Rect::new(100, 800 - 24 - 560, 400, 648));
        assert_eq!(group_rect((100, 100), PILL, PANEL, G, None), Rect::new(100, 100, 400, 64));
    }

    #[test]
    fn stays_on_screen() {
        assert_eq!(clamp_and_snap((-50, -30), (360, 440), WORK, 1.0, false), (0, 0));
        assert_eq!(clamp_and_snap((1900, 1140), (360, 440), WORK, 1.0, false), (1560, 712));
        assert_eq!(clamp_and_snap((500, 300), (360, 440), WORK, 1.0, false), (500, 300));
        // grösser als der Monitor: oben links
        assert_eq!(clamp_and_snap((100, 100), (3000, 2000), WORK, 1.0, false), (0, 0));
    }

    #[test]
    fn snaps_to_edges_with_margin() {
        // links oben
        assert_eq!(clamp_and_snap((10, 10), (400, 64), WORK, 1.0, true), (12, 12));
        // rechts unten (der Arbeitsbereich schliesst die Taskleiste schon aus)
        assert_eq!(clamp_and_snap((1510, 1080), (400, 64), WORK, 1.0, true), (1508, 1076));
        // in der Mitte bleibt es stehen
        assert_eq!(clamp_and_snap((500, 300), (400, 64), WORK, 1.0, true), (500, 300));
        // mit Skalierung 2 verdoppeln sich Abstand und Reichweite
        assert_eq!(clamp_and_snap((30, 30), (800, 128), WORK, 2.0, true), (24, 24));
    }

    #[test]
    fn works_on_a_second_monitor() {
        let left = Rect::new(-1920, 0, 1920, 1080);
        assert_eq!(clamp_and_snap((-1915, 5), (400, 64), left, 1.0, true), (-1908, 12));
    }

    #[test]
    fn settle_keeps_the_whole_group_on_the_monitor() {
        // aufgeklappt nach unten, Pille nahe am unteren Rand gezogen: die Gruppe wird hochgeschoben
        let pill = settle_pill((500, 900), PILL, PANEL, G, Some(Dir::Down), WORK, 1.0, false);
        assert_eq!(pill, (500, WORK.bottom() - 648));
        // nach oben: Panel ragt oben hinaus, die Pille rückt runter
        let pill = settle_pill((500, 100), PILL, PANEL, G, Some(Dir::Up), WORK, 1.0, false);
        assert_eq!(pill, (500, 584));
        // eingeklappt zählt nur die Pille
        let pill = settle_pill((500, 1100), PILL, PANEL, G, None, WORK, 1.0, false);
        assert_eq!(pill, (500, 1088));
        // Einrasten wirkt auf die Gruppe: Panel-Oberkante nahe am oberen Rand
        let pill = settle_pill((500, 584 + 8), PILL, PANEL, G, Some(Dir::Up), WORK, 1.0, true);
        assert_eq!(pill, (500, 584 + 12));
    }

    // --- Popover ---

    const POP: (i32, i32) = (760, 620);

    #[test]
    fn popover_prefers_the_right_side() {
        // Leiste links auf dem Bildschirm, aufgeklappt (648 hoch)
        let bar = Rect::new(100, 100, 400, 648);
        let (x, y) = anchor_popover(bar, POP, WORK, G);
        assert_eq!(x, 500 + 24);
        let pop = Rect::new(x, y, POP.0, POP.1);
        assert!(pop.inside(&WORK) && !pop.intersects(&bar));
        // senkrecht zur Leiste zentriert
        assert_eq!(y, 100 + (648 - 620) / 2);
    }

    #[test]
    fn popover_goes_left_when_the_right_side_is_too_narrow() {
        // Leiste am rechten Rand (typisch)
        let bar = Rect::new(1920 - 12 - 400, 300, 400, 648);
        let (x, y) = anchor_popover(bar, POP, WORK, G);
        assert_eq!(x, bar.x - 24 - 760);
        let pop = Rect::new(x, y, POP.0, POP.1);
        assert!(pop.inside(&WORK) && !pop.intersects(&bar));
    }

    #[test]
    fn popover_is_pushed_up_so_it_stays_on_the_monitor() {
        // Pille unten, Panel oben: die Leiste ist hoch, das Popover seitlich wird nicht über den Rand gesetzt
        let bar = Rect::new(1508, 500, 400, 648);
        let (x, y) = anchor_popover(bar, (760, 700), WORK, G);
        let pop = Rect::new(x, y, 760, 700);
        assert!(pop.inside(&WORK), "{pop:?}");
        assert!(!pop.intersects(&bar));
        // eingeklappte Pille ganz unten: seitlich zentriert, aber auf den Monitor geschoben
        let pill = Rect::new(1508, 1076, 400, 64);
        let (x, y) = anchor_popover(pill, POP, WORK, G);
        assert_eq!(x, 1508 - 24 - 760);
        assert_eq!(y, 1152 - 620);
    }

    #[test]
    fn popover_goes_below_or_above_on_a_narrow_monitor() {
        // 1000 breit: weder rechts noch links neben einer mittigen Leiste ist Platz für 760
        let work = Rect::new(0, 0, 1000, 1152);
        let pill = Rect::new(300, 100, 400, 64);
        let (x, y) = anchor_popover(pill, POP, work, G);
        assert_eq!((x, y), (120, 100 + 64 + 24));
        let pop = Rect::new(x, y, POP.0, POP.1);
        assert!(pop.inside(&work) && !pop.intersects(&pill));
        // Pille unten: darüber
        let pill = Rect::new(300, 1000, 400, 64);
        let (x, y) = anchor_popover(pill, POP, work, G);
        assert_eq!((x, y), (120, 1000 - 24 - 620));
        assert!(Rect::new(x, y, POP.0, POP.1).inside(&work));
    }

    #[test]
    fn popover_on_a_second_monitor_uses_its_own_work_area() {
        let right = Rect::new(1920, 0, 1920, 1080);
        let bar = Rect::new(1920 + 12, 12, 400, 648);
        let (x, _) = anchor_popover(bar, POP, right, G);
        assert_eq!(x, 1920 + 12 + 400 + 24);
        let left = Rect::new(-1920, 0, 1920, 1080);
        let bar = Rect::new(-1920 + 12, 100, 400, 64);
        let (x, y) = anchor_popover(bar, POP, left, G);
        assert_eq!(x, -1920 + 12 + 400 + 24);
        assert!(Rect::new(x, y, POP.0, POP.1).inside(&left));
    }

    #[test]
    fn popover_never_leaves_the_monitor_even_if_nothing_fits() {
        let work = Rect::new(0, 0, 800, 640);
        let bar = Rect::new(200, 0, 400, 648);
        let (x, y) = anchor_popover(bar, POP, work, G);
        // überlappt zwar die Leiste, liegt aber ganz auf dem Monitor
        assert_eq!((x, y), (0, 14));
        assert!(Rect::new(x, y, POP.0, POP.1).inside(&Rect::new(0, 0, 800.max(POP.0), 640)));
    }

    #[test]
    fn reminder_sits_bottom_right_unless_the_bar_is_there() {
        let size = (400, 140);
        // keine Leiste oder Leiste woanders: unten rechts
        assert_eq!(reminder_position(WORK, size, 12, None, 24), (1508, 1000));
        let far = Rect::new(100, 100, 400, 64);
        assert_eq!(reminder_position(WORK, size, 12, Some(far), 24), (1508, 1000));
        // Leiste ebenfalls unten rechts (Standard): das Hinweisfenster steht links daneben
        let bar = Rect::new(1508, 492, 400, 648);
        let (x, y) = reminder_position(WORK, size, 12, Some(bar), 24);
        assert_eq!(x, 1508 - 24 - 400);
        let rect = Rect::new(x, y, 400, 140);
        assert!(rect.inside(&WORK) && !rect.intersects(&bar));
        // eingeklappte Pille unten rechts
        let pill = Rect::new(1508, 1076, 400, 64);
        let (x, y) = reminder_position(WORK, size, 12, Some(pill), 24);
        let rect = Rect::new(x, y, 400, 140);
        assert!(rect.inside(&WORK) && !rect.intersects(&pill), "{rect:?}");
    }
}
