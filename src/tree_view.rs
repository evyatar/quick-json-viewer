//! Virtualized row renderer for the Viewer tree and the Compare diff tree.
//!
//! One custom iced widget draws only the rows intersecting the scroll
//! viewport, so documents with millions of nodes stay cheap: layout is a
//! single `rows × row_height` box, and painting/hit-testing is O(visible).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::{self, Quad, Renderer as _};
use iced::advanced::text::{self, Paragraph as _, Renderer as _};
use iced::advanced::widget::{self, Tree, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::alignment::Vertical;
use iced::{mouse, Border, Color, Element, Event, Font, Length, Point, Rectangle, Shadow, Size};

use crate::diff::{DiffNode, DiffResult, DiffStatus};
use crate::export::{self, AddedItem, NodeEdit};
use crate::index::{JsonIndex, Node, NodeKind, NodeSet};
use crate::theme;

/// Where inside a row a click landed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    Checkbox,
    Caret,
    Key,
    Other,
}

#[derive(Clone, Debug)]
pub enum RowEvent {
    Click { node: u32, region: Region },
    DoubleClick { node: u32, region: Region },
    RightClick { node: u32, position: Point },
}

/// Everything the viewer rows need to paint themselves.
pub struct ViewerRows<'a> {
    pub index:             &'a JsonIndex,
    pub added_items:       &'a [AddedItem],
    pub expanded:          &'a NodeSet,
    pub selected:          Option<u32>,
    pub search_result_set: &'a NodeSet,
    pub visible:           &'a [u32],
    pub edit_overlay:      &'a HashMap<u32, NodeEdit>,
    pub saved_overlay:     &'a HashMap<u32, NodeEdit>,
    pub multi_select:      bool,
    pub checked:           &'a HashSet<u32>,
}

/// Everything the compare rows need to paint themselves.
pub struct DiffRows<'a> {
    pub result:   &'a DiffResult,
    pub expanded: &'a NodeSet,
    pub selected: Option<u32>,
    pub visible:  &'a [u32],
}

pub enum Rows<'a> {
    Viewer(ViewerRows<'a>),
    Diff(DiffRows<'a>),
}

impl Rows<'_> {
    fn len(&self) -> usize {
        match self {
            Rows::Viewer(v) => v.visible.len(),
            Rows::Diff(d) => d.visible.len(),
        }
    }
    fn node_at(&self, row: usize) -> u32 {
        match self {
            Rows::Viewer(v) => v.visible[row],
            Rows::Diff(d) => d.visible[row],
        }
    }
}

pub struct TreeView<'a, Message> {
    rows:       Rows<'a>,
    row_h:      f32,
    key_font:   Font,
    val_font:   Font,
    font_size:  f32,
    dark:       bool,
    /// Bumped whenever a different document is shown, so the cached content
    /// width starts over instead of carrying the previous file's extent.
    generation: u64,
    on_event:   Box<dyn Fn(RowEvent) -> Message + 'a>,
}

impl<'a, Message> TreeView<'a, Message> {
    pub fn new(
        rows: Rows<'a>,
        row_h: f32,
        key_font: Font,
        val_font: Font,
        font_size: f32,
        dark: bool,
        generation: u64,
        on_event: impl Fn(RowEvent) -> Message + 'a,
    ) -> Self {
        Self { rows, row_h, key_font, val_font, font_size, dark, generation, on_event: Box::new(on_event) }
    }
}

struct State {
    generation:  u64,
    hovered:     Option<usize>,
    last_click:  Option<(Instant, usize)>,
    /// Widest row content painted so far (monotonic per generation). Written
    /// from `draw`, hence the `Cell`.
    content_w:   Cell<f32>,
    /// Width of the visible area, learned from the events' viewport.
    viewport_w:  f32,
    /// Width the last `layout` produced; a mismatch triggers a relayout.
    layout_w:    f32,
    /// Whether the last layout had an unbounded width (horizontal scrolling),
    /// in which case the widget sizes itself from `viewport_w` / `content_w`.
    unbounded:   bool,
    /// Paragraphs shaped during the current frame. The renderer only holds a
    /// weak reference to a paragraph until the frame is presented, so they
    /// must outlive `draw`; the vector is recycled on the next frame.
    paragraphs:  RefCell<Vec<Paragraph>>,
}

impl State {
    fn new(generation: u64) -> Self {
        Self {
            generation,
            hovered: None,
            last_click: None,
            content_w: Cell::new(0.0),
            viewport_w: 0.0,
            layout_w: 0.0,
            unbounded: false,
            paragraphs: RefCell::new(Vec::new()),
        }
    }

    /// Width we want when horizontally scrollable: at least the visible
    /// area, widened to the widest row painted so far.
    fn unbounded_width(&self) -> f32 {
        self.viewport_w.max(self.content_w.get()).max(1.0)
    }
}

type Renderer = iced::Renderer;
type Paragraph = <Renderer as text::Renderer>::Paragraph;

const CARET_W: f32 = 18.0;
const INDENT_STEP: f32 = 16.0;
const CHECKBOX_W: f32 = 20.0;
const DOUBLE_CLICK: Duration = Duration::from_millis(450);

fn paragraph(content: &str, font: Font, size: f32, row_h: f32) -> Paragraph {
    Paragraph::with_text(text::Text {
        content,
        bounds: Size::new(100_000.0, row_h),
        size: size.into(),
        line_height: text::LineHeight::default(),
        font,
        // Not `Left`: iced 0.14 skips the RTL relayout for single-line
        // paragraphs with an explicit alignment, so RTL text (e.g. Hebrew)
        // stays right-aligned to the 100k-wide bounds and is drawn off-screen.
        align_x: text::Alignment::Default,
        align_y: Vertical::Top,
        shaping: text::Shaping::Advanced,
        wrapping: text::Wrapping::None,
    })
}

fn fill_rect(renderer: &mut Renderer, bounds: Rectangle, color: Color) {
    renderer.fill_quad(
        Quad { bounds, border: Border::default(), shadow: Shadow::default(), snap: true },
        color,
    );
}

/// Draw `content` left-aligned at `x`, vertically centred on `y`; returns its
/// width. The shaped paragraph is parked in `keep` until the frame is shown.
#[allow(clippy::too_many_arguments)]
fn draw_text(
    renderer: &mut Renderer,
    keep: &mut Vec<Paragraph>,
    content: &str,
    font: Font,
    size: f32,
    row_h: f32,
    color: Color,
    x: f32,
    y: f32,
    clip: Rectangle,
) -> f32 {
    if content.is_empty() {
        return 0.0;
    }
    let p = paragraph(content, font, size, row_h);
    let min = p.min_bounds();
    // `fill_paragraph` takes the top-left corner; centre on `y` ourselves.
    renderer.fill_paragraph(&p, Point::new(x, y - min.height / 2.0), color, clip);
    keep.push(p);
    min.width
}

fn measure(content: &str, font: Font, size: f32, row_h: f32) -> f32 {
    if content.is_empty() { 0.0 } else { paragraph(content, font, size, row_h).min_width() }
}

// ─── shared text/colour helpers ──────────────────────────────────────────────

/// Key (or array-index) display text + colour for a node.
pub fn key_parts(index: &JsonIndex, node: &Node, dark: bool) -> (String, Color) {
    if node.key_len > 0 {
        (format!("\"{}\"", index.key_of(node)), if dark { theme::KEY } else { theme::LIGHT_KEY })
    } else if node.array_index != u32::MAX {
        (format!("{}", node.array_index), if dark { theme::ARRAY_INDEX } else { theme::LIGHT_ARRAY_INDEX })
    } else {
        (String::new(), Color::TRANSPARENT)
    }
}

/// Value display text + colour for a node (containers show their child count;
/// long strings are truncated to 500 chars).
pub fn value_parts(index: &JsonIndex, node: &Node, dark: bool) -> (String, Color) {
    let str_color       = if dark { theme::STRING }    else { theme::LIGHT_STRING };
    let container_color = if dark { theme::CONTAINER } else { theme::LIGHT_CONTAINER };
    match node.kind {
        NodeKind::Object => (format!("{{ {} }}", node.child_count), container_color),
        NodeKind::Array  => (format!("[ {} ]",   node.child_count), container_color),
        NodeKind::String => {
            let raw = index.value_bytes(node);
            let inner = if raw.len() >= 2 { &raw[1..raw.len() - 1] } else { raw };
            let text = String::from_utf8_lossy(inner);
            let s = match text.char_indices().nth(500) {
                Some((cut, _)) => format!("\"{}…\"", &text[..cut]),
                None           => format!("\"{}\"", text),
            };
            (s, str_color)
        }
        NodeKind::Number => {
            let raw = index.value_bytes(node);
            (String::from_utf8_lossy(raw).into_owned(), if dark { theme::NUMBER } else { theme::LIGHT_NUMBER })
        }
        NodeKind::Bool => {
            let raw = index.value_bytes(node);
            (String::from_utf8_lossy(raw).into_owned(), if dark { theme::BOOL } else { theme::LIGHT_BOOL })
        }
        NodeKind::Null => ("null".to_owned(), if dark { theme::NULL } else { theme::LIGHT_CONTAINER }),
    }
}

fn sep_color(dark: bool) -> Color {
    if dark { theme::PUNCT } else { theme::LIGHT_PUNCT }
}

/// A pending (not-yet-saved) added item has no real node — fabricate one with
/// just enough fields for geometry/rendering. Its byte offsets stay zeroed, so
/// `index.value_bytes` / `index.key_of` return empty if ever called on it.
pub fn synthetic_node(index: &JsonIndex, added_items: &[AddedItem], node_idx: u32) -> Node {
    let item = &added_items[node_idx as usize - index.nodes.len()];
    Node {
        kind:         export::added_kind(&item.raw_value),
        depth:        export::added_depth(&index.nodes, added_items, node_idx),
        value_start:  0,
        value_end:    0,
        key_start:    0,
        key_len:      0,
        next_sibling: u32::MAX,
        child_count:  export::added_child_count(added_items, node_idx),
        parent:       item.parent,
        array_index:  export::added_display_index(&index.nodes, added_items, node_idx),
    }
}

/// Resolved display data for one viewer row.
struct ViewerRow {
    node:        Node,
    is_new:      bool,
    key_text:    String,
    key_color:   Color,
    value_text:  String,
    value_color: Color,
    is_deleted:  bool,
    has_edit:    bool,
    can_toggle:  bool,
    is_expanded: bool,
    is_selected: bool,
    is_match:    bool,
    can_check:   bool,
    is_checked:  bool,
    is_container: bool,
}

impl ViewerRows<'_> {
    fn resolve(&self, node_idx: u32, dark: bool) -> ViewerRow {
        let index = self.index;
        let is_new = export::is_added(index.nodes.len(), node_idx);
        let node = if is_new {
            synthetic_node(index, self.added_items, node_idx)
        } else {
            index.nodes[node_idx as usize]
        };
        let kind = node.kind;
        let is_container = matches!(kind, NodeKind::Object | NodeKind::Array);
        let has_children = node.child_count > 0;
        let can_toggle = is_container && has_children && node_idx != index.root;

        let (mut key_text, mut key_color) = key_parts(index, &node, dark);
        if is_new {
            if let Some(k) = &self.added_items[node_idx as usize - index.nodes.len()].key {
                key_text = format!("\"{}\"", k);
            }
        }
        let (mut value_text, mut value_color) = if is_new {
            let text = match kind {
                NodeKind::Object => format!("{{ {} }}", node.child_count),
                NodeKind::Array  => format!("[ {} ]", node.child_count),
                _ => self.added_items[node_idx as usize - index.nodes.len()].raw_value.clone(),
            };
            (text, theme::NEW)
        } else {
            value_parts(index, &node, dark)
        };

        let pending = self.edit_overlay.get(&node_idx) != self.saved_overlay.get(&node_idx);
        let is_deleted = self.edit_overlay.get(&node_idx).map_or(false, |e| e.deleted);
        if let Some(ov) = self.edit_overlay.get(&node_idx) {
            if let Some(k) = &ov.key_override {
                key_text = format!("\"{}\"", k);
                if pending { key_color = theme::ACCENT; }
            }
            if let Some(v) = &ov.value_override {
                value_text = v.clone();
                if pending && !is_new { value_color = theme::ACCENT; }
            }
        }
        if is_new && !is_deleted {
            key_color = theme::NEW;
            value_color = theme::NEW;
        }
        if is_deleted {
            key_color = theme::DELETED;
            value_color = theme::DELETED;
        }

        ViewerRow {
            node,
            is_new,
            key_text,
            key_color,
            value_text,
            value_color,
            is_deleted,
            has_edit: pending && !is_new,
            can_toggle,
            is_expanded: self.expanded.contains(&node_idx),
            is_selected: self.selected == Some(node_idx),
            is_match: self.search_result_set.contains(&node_idx),
            can_check: self.multi_select && is_container && !is_new,
            is_checked: self.checked.contains(&node_idx),
            is_container,
        }
    }

    fn checkbox_w(&self) -> f32 {
        if self.multi_select { CHECKBOX_W } else { 0.0 }
    }

    fn indent_at(&self, depth: u16) -> f32 {
        self.checkbox_w() + 4.0 + depth as f32 * INDENT_STEP
    }
}

impl<'a, Message> Widget<Message, iced::Theme, Renderer> for TreeView<'a, Message> {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Shrink)
    }

    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<State>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(State::new(self.generation))
    }

    fn diff(&self, tree: &mut Tree) {
        let st = tree.state.downcast_mut::<State>();
        if st.generation != self.generation {
            let viewport_w = st.viewport_w;
            *st = State::new(self.generation);
            st.viewport_w = viewport_w;
        }
    }

    fn layout(&mut self, tree: &mut Tree, _renderer: &Renderer, limits: &layout::Limits) -> layout::Node {
        let st = tree.state.downcast_mut::<State>();
        let max_w = limits.max().width;
        st.unbounded = !max_w.is_finite();
        let width = if st.unbounded {
            // Inside a horizontally scrollable container: size ourselves.
            st.unbounded_width().max(limits.min().width)
        } else {
            // Vertical-only scrolling: fill the container exactly.
            max_w.max(limits.min().width).max(1.0)
        };
        st.layout_w = width;
        let height = (self.rows.len() as f32 * self.row_h).max(1.0);
        layout::Node::new(Size::new(width, height))
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let st = tree.state.downcast_mut::<State>();
        let bounds = layout.bounds();

        // Track the visible width and relayout when our box no longer covers
        // it (or when a wider row has been painted since the last layout).
        if viewport.width > 0.0 && (viewport.width - st.viewport_w).abs() > 0.5 {
            st.viewport_w = viewport.width;
        }
        if st.unbounded && (st.unbounded_width() - st.layout_w).abs() > 0.5 {
            shell.invalidate_layout();
        }

        let row_at = |p: Point| -> Option<usize> {
            if !bounds.contains(p) { return None; }
            let row = ((p.y - bounds.y) / self.row_h).floor();
            if row < 0.0 { return None; }
            let row = row as usize;
            (row < self.rows.len()).then_some(row)
        };

        match event {
            Event::Mouse(mouse::Event::CursorMoved { .. }) | Event::Mouse(mouse::Event::CursorLeft) => {
                let hovered = cursor.position().and_then(row_at);
                if hovered != st.hovered {
                    st.hovered = hovered;
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(button)) => {
                let Some(pos) = cursor.position() else { return };
                let Some(row) = row_at(pos) else { return };
                let node = self.rows.node_at(row);
                match button {
                    mouse::Button::Left => {
                        let now = Instant::now();
                        let is_double = matches!(st.last_click, Some((t, r)) if r == row && now.duration_since(t) < DOUBLE_CLICK);
                        st.last_click = if is_double { None } else { Some((now, row)) };
                        let region = self.region_at(node, pos.x - bounds.x, bounds.width);
                        let ev = if is_double {
                            RowEvent::DoubleClick { node, region }
                        } else {
                            RowEvent::Click { node, region }
                        };
                        shell.publish((self.on_event)(ev));
                        shell.capture_event();
                    }
                    mouse::Button::Right => {
                        shell.publish((self.on_event)(RowEvent::RightClick { node, position: pos }));
                        shell.capture_event();
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        if cursor.is_over(layout.bounds()) { mouse::Interaction::Idle } else { mouse::Interaction::None }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _theme: &iced::Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let st = tree.state.downcast_ref::<State>();
        let bounds = layout.bounds();
        let Some(clip) = bounds.intersection(viewport) else { return };
        let n = self.rows.len();
        if n == 0 { return; }

        let first = ((clip.y - bounds.y) / self.row_h).floor().max(0.0) as usize;
        let last = (((clip.y + clip.height - bounds.y) / self.row_h).ceil() as usize).min(n);

        let mut keep = st.paragraphs.borrow_mut();
        keep.clear();
        let mut max_w = st.content_w.get();
        for row in first..last {
            let y = bounds.y + row as f32 * self.row_h;
            let rect = Rectangle { x: bounds.x, y, width: bounds.width, height: self.row_h };
            let hovered = st.hovered == Some(row);
            let w = match &self.rows {
                Rows::Viewer(v) => self.draw_viewer_row(renderer, &mut keep, v, row, rect, hovered, clip),
                Rows::Diff(d)   => self.draw_diff_row(renderer, &mut keep, d, row, rect, hovered, clip),
            };
            if w > max_w { max_w = w; }
        }
        if max_w > st.content_w.get() {
            st.content_w.set(max_w);
        }
    }
}

impl<'a, Message> TreeView<'a, Message> {
    /// Classify a click's horizontal offset within the row.
    fn region_at(&self, node_idx: u32, x: f32, row_width: f32) -> Region {
        match &self.rows {
            Rows::Viewer(v) => {
                let index = v.index;
                let is_new = export::is_added(index.nodes.len(), node_idx);
                let node = if is_new { synthetic_node(index, v.added_items, node_idx) } else { index.nodes[node_idx as usize] };
                let is_container = matches!(node.kind, NodeKind::Object | NodeKind::Array);
                let can_toggle = is_container && node.child_count > 0 && node_idx != index.root;
                let can_check = v.multi_select && is_container && !is_new;
                if can_check && x >= 2.0 && x < 2.0 + CHECKBOX_W {
                    return Region::Checkbox;
                }
                let indent = v.indent_at(node.depth);
                if can_toggle && x >= indent && x < indent + 16.0 {
                    return Region::Caret;
                }
                if !is_container {
                    let row = v.resolve(node_idx, self.dark);
                    let key_w = measure(&row.key_text, self.key_font, self.font_size, self.row_h);
                    let key_x0 = indent + CARET_W;
                    if key_w > 0.0 && x >= key_x0 && x < key_x0 + key_w {
                        return Region::Key;
                    }
                }
                Region::Other
            }
            Rows::Diff(d) => {
                let dn = &d.result.nodes[node_idx as usize];
                let can_toggle = dn.child_count > 0 && node_idx != d.result.root;
                if !can_toggle { return Region::Other; }
                let indent = 4.0 + dn.depth as f32 * INDENT_STEP;
                let mid = row_width / 2.0;
                let in_left = x >= indent && x < indent + 16.0;
                let in_right = x >= mid + indent && x < mid + indent + 16.0;
                if in_left || in_right { Region::Caret } else { Region::Other }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_viewer_row(
        &self,
        renderer: &mut Renderer,
        keep: &mut Vec<Paragraph>,
        v: &ViewerRows<'_>,
        row: usize,
        rect: Rectangle,
        hovered: bool,
        clip: Rectangle,
    ) -> f32 {
        let node_idx = v.visible[row];
        let r = v.resolve(node_idx, self.dark);
        let dark = self.dark;
        let pal = theme::Palette::for_dark(dark);
        let row_h = self.row_h;
        let fs = self.font_size;
        let depth = r.node.depth;
        let indent = v.indent_at(depth);

        // Background: selection/hover first, then the translucent match
        // highlight on top so it stays visible over the opaque hover fill.
        if r.is_selected {
            if dark {
                fill_rect(renderer, rect, theme::SELECTION_BG);
                fill_rect(renderer, Rectangle { width: 2.0, ..rect }, theme::ACCENT);
            } else {
                fill_rect(renderer, rect, theme::LIGHT_SELECTION_BG);
            }
        } else if hovered {
            fill_rect(renderer, rect, pal.hover_bg);
        }
        // Indent guides go under the match highlight so the translucent
        // yellow tints them instead of leaving dark stripes across the row.
        if dark {
            for d in 0..depth {
                let gx = rect.x + v.indent_at(d) + 8.0;
                fill_rect(renderer, Rectangle { x: gx, y: rect.y, width: 1.0, height: row_h }, theme::INDENT_GUIDE);
            }
        }
        if r.is_match {
            fill_rect(renderer, rect, if dark { theme::MATCH_BG } else { theme::MATCH_BG_LIGHT });
        }

        let y1 = rect.y + row_h / 2.0;
        let text_col = pal.text_primary;

        // Checkbox gutter
        if r.can_check {
            let glyph = if r.is_checked { "☑" } else { "☐" };
            let col = if r.is_checked { theme::ACCENT } else if dark { theme::TEXT_FAINT } else { text_col };
            draw_text(renderer, keep, glyph, self.val_font, fs, row_h, col, rect.x + 4.0, y1, clip);
        }

        let mut x = rect.x + indent;
        if r.can_toggle {
            let tri = if r.is_expanded { "▼" } else { "▶" };
            let tri_col = if dark { theme::TEXT_FAINT } else { text_col };
            draw_text(renderer, keep, tri, self.val_font, (fs - 3.0).max(8.0), row_h, tri_col, x + 2.0, y1, clip);
        }
        x += CARET_W;
        let text_x0 = x;

        let key_w = draw_text(renderer, keep, &r.key_text, self.key_font, fs, row_h, r.key_color, x, y1, clip);
        x += key_w;
        let sep_w = if r.key_text.is_empty() {
            0.0
        } else {
            draw_text(renderer, keep, " : ", self.key_font, fs, row_h, sep_color(dark), x, y1, clip)
        };
        x += sep_w;
        let val_w = draw_text(renderer, keep, &r.value_text, self.val_font, fs, row_h, r.value_color, x, y1, clip);
        x += val_w;

        if r.has_edit {
            let dot = Rectangle { x: rect.x + rect.width - 9.0, y: y1 - 3.0, width: 6.0, height: 6.0 };
            renderer.fill_quad(
                Quad { bounds: dot, border: Border { radius: 3.0.into(), ..Border::default() }, shadow: Shadow::default(), snap: false },
                theme::ACCENT,
            );
        }
        if r.is_deleted {
            fill_rect(
                renderer,
                Rectangle { x: text_x0, y: y1 - 0.75, width: key_w + sep_w + val_w, height: 1.5 },
                theme::DELETED,
            );
        }
        let _ = r.is_container;
        let _ = r.is_new;
        x - rect.x + 8.0
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_diff_row(
        &self,
        renderer: &mut Renderer,
        keep: &mut Vec<Paragraph>,
        d: &DiffRows<'_>,
        row: usize,
        rect: Rectangle,
        hovered: bool,
        clip: Rectangle,
    ) -> f32 {
        let node_idx = d.visible[row];
        let dn: &DiffNode = &d.result.nodes[node_idx as usize];
        let dark = self.dark;
        let pal = theme::Palette::for_dark(dark);
        let is_expanded = d.expanded.contains(&node_idx);
        let is_selected = d.selected == Some(node_idx);
        let can_toggle = dn.child_count > 0 && node_idx != d.result.root;

        let mid_x = rect.x + rect.width / 2.0;
        let left_cell = Rectangle { x: rect.x, y: rect.y, width: rect.width / 2.0, height: rect.height };
        let right_cell = Rectangle { x: mid_x, y: rect.y, width: rect.width / 2.0, height: rect.height };

        if !is_selected && hovered {
            fill_rect(renderer, rect, pal.hover_bg);
        }

        let tint_status = if is_expanded && dn.child_count > 0 { DiffStatus::Unchanged } else { dn.status };
        let (lt, rt) = match tint_status {
            DiffStatus::Removed   => (Some(theme::DIFF_REMOVED_BG), Some(theme::DIFF_EMPTY_BG)),
            DiffStatus::Added     => (Some(theme::DIFF_EMPTY_BG),   Some(theme::DIFF_ADDED_BG)),
            DiffStatus::Changed   => (Some(theme::DIFF_CHANGED_BG), Some(theme::DIFF_CHANGED_BG)),
            DiffStatus::Unchanged => (None, None),
        };
        if let Some(c) = lt { fill_rect(renderer, left_cell, c); }
        if let Some(c) = rt { fill_rect(renderer, right_cell, c); }

        if is_selected {
            fill_rect(renderer, rect, theme::DIFF_SELECTION_OVERLAY);
            fill_rect(renderer, Rectangle { width: 2.0, ..rect }, theme::ACCENT);
        }

        // Centre divider
        fill_rect(renderer, Rectangle { x: mid_x, y: rect.y, width: 1.0, height: rect.height }, pal.border);

        let text_col = pal.text_primary;
        let left = &*d.result.left;
        let right = &*d.result.right;
        let mut w = 0.0f32;
        if let Some(li) = dn.left_idx() {
            if let Some(cell_clip) = left_cell.intersection(&clip) {
                w = w.max(self.draw_diff_cell(renderer, keep, left_cell, left, &left.nodes[li as usize], dn.depth, can_toggle, is_expanded, text_col, cell_clip));
            }
        }
        if let Some(ri) = dn.right_idx() {
            if let Some(cell_clip) = right_cell.intersection(&clip) {
                w = w.max(self.draw_diff_cell(renderer, keep, right_cell, right, &right.nodes[ri as usize], dn.depth, can_toggle, is_expanded, text_col, cell_clip));
            }
        }
        // The diff view never scrolls horizontally; report the container width.
        let _ = w;
        0.0
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_diff_cell(
        &self,
        renderer: &mut Renderer,
        keep: &mut Vec<Paragraph>,
        cell: Rectangle,
        index: &JsonIndex,
        node: &Node,
        depth: u16,
        show_caret: bool,
        is_expanded: bool,
        text_col: Color,
        clip: Rectangle,
    ) -> f32 {
        let dark = self.dark;
        let row_h = self.row_h;
        let fs = self.font_size;
        let (key_text, key_color) = key_parts(index, node, dark);
        let (value_text, value_color) = value_parts(index, node, dark);

        let indent = 4.0 + depth as f32 * INDENT_STEP;
        let y1 = cell.y + row_h / 2.0;

        if dark {
            for d in 0..depth {
                let gx = cell.x + 4.0 + d as f32 * INDENT_STEP + 8.0;
                fill_rect(renderer, Rectangle { x: gx, y: cell.y, width: 1.0, height: row_h }, theme::INDENT_GUIDE);
            }
        }

        let mut x = cell.x + indent;
        if show_caret {
            let tri = if is_expanded { "▼" } else { "▶" };
            let tri_col = if dark { theme::TEXT_FAINT } else { text_col };
            draw_text(renderer, keep, tri, self.val_font, (fs - 3.0).max(8.0), row_h, tri_col, x + 2.0, y1, clip);
        }
        x += CARET_W;

        if !key_text.is_empty() {
            x += draw_text(renderer, keep, &key_text, self.key_font, fs, row_h, key_color, x, y1, clip);
            x += draw_text(renderer, keep, " : ", self.key_font, fs, row_h, sep_color(dark), x, y1, clip);
        }
        x += draw_text(renderer, keep, &value_text, self.val_font, fs, row_h, value_color, x, y1, clip);
        x - cell.x
    }
}

impl<'a, Message: 'a> From<TreeView<'a, Message>> for Element<'a, Message> {
    fn from(view: TreeView<'a, Message>) -> Self {
        Element::new(view)
    }
}
