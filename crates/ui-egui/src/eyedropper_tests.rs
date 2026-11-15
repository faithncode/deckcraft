//! The Eyedropper on the slide canvas through real egui input.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use deckcraft_engine::Session;
use egui::{Event, Id, Key, Modifiers, PointerButton, Pos2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

use crate::eyedropper::{note_open_menu, request, result_waits_for, take_result};
use crate::{Services, SlideApp};

fn harness() -> Harness<'static, SlideApp> {
    let mut session = Session::with_new();
    session.execute("slide.new", &json!({"layout": "blank"})).unwrap();
    session.execute("shape.insert", &json!({"preset": "rect", "rect": [100, 100, 200, 100]})).unwrap();
    session.execute("shape.fill", &json!({"color": "#ff0000"})).unwrap();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_ui_state(
        |ui, app: &mut SlideApp| {
            app.logic(ui.ctx());
            app.ui(ui);
        },
        SlideApp::new(session, Services::default()),
    );
    h.run_steps(4);
    h
}

/// Test helper: stable unique id for the eyedropper test menu.
fn menu() -> Id {
    Id::unique("test-menu")
}

/// A colour menu asked for the Eyedropper.
fn ask(h: &mut Harness<'static, SlideApp>) {
    note_open_menu(&h.ctx, menu());
    request(&h.ctx);
    h.run_steps(3);
}

/// Where a point of the slide is on screen.
fn at(h: &Harness<'static, SlideApp>, x: f32, y: f32) -> Pos2 {
    let app = h.state();
    let r = app.slide_rect.expect("the slide is drawn");
    r.min + egui::vec2(x, y) * app.canvas_scale
}

fn click(h: &mut Harness<'static, SlideApp>, pos: Pos2) {
    h.input_mut().events.push(Event::PointerMoved(pos));
    h.run_steps(2);
    for pressed in [true, false] {
        h.input_mut().events.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
        h.run_steps(2);
    }
    h.run_steps(2);
}

fn selection(h: &Harness<'static, SlideApp>) -> Vec<u32> {
    h.state().session.doc().unwrap().selection.shapes.iter().map(|s| s.0).collect()
}

#[test]
fn a_click_on_the_slide_takes_the_colour_under_it_and_nothing_else_happens() {
    let mut h = harness();
    let before = selection(&h);
    let revision = h.state().session.doc().unwrap().revision;
    ask(&mut h);
    assert!(h.state().eyedropper.is_some(), "asking starts the Eyedropper");
    // A hover shows the colour; nothing is chosen yet.
    let on_shape = at(&h, 150.0, 150.0);
    h.input_mut().events.push(Event::PointerMoved(on_shape));
    h.run_steps(3);
    assert!(h.state().eyedropper.is_some() && !result_waits_for(&h.ctx, menu()));
    click(&mut h, on_shape);
    assert!(h.state().eyedropper.is_none(), "one click ends it");
    assert!(result_waits_for(&h.ctx, menu()), "the colour waits for the menu that asked");
    assert_eq!(take_result(&h.ctx).as_deref(), Some("FF0000"), "the red of the shape under the pointer");
    assert_eq!(take_result(&h.ctx), None, "taken once");
    assert_eq!(selection(&h), before, "the click did not select or move anything");
    // The slide itself is untouched, and the next click is an ordinary click again.
    assert_eq!(h.state().session.doc().unwrap().revision, revision, "the deck is untouched");
    // Beside the shape the same click takes the slide's background instead.
    ask(&mut h);
    let empty = at(&h, 700.0, 450.0);
    click(&mut h, empty);
    let bg = take_result(&h.ctx).expect("a colour");
    assert_ne!(bg, "FF0000");
}

#[test]
fn escape_and_a_click_off_the_slide_take_nothing() {
    let mut h = harness();
    ask(&mut h);
    let on_shape = at(&h, 150.0, 150.0);
    h.input_mut().events.push(Event::PointerMoved(on_shape));
    h.run_steps(2);
    for pressed in [true, false] {
        h.input_mut().events.push(Event::Key { key: Key::Escape, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE });
        h.run_steps(2);
    }
    assert!(h.state().eyedropper.is_none(), "Esc cancels");
    assert_eq!(take_result(&h.ctx), None);

    ask(&mut h);
    let off = h.state().slide_rect.unwrap().min - egui::vec2(10.0, 10.0);
    click(&mut h, off);
    assert!(h.state().eyedropper.is_none(), "a click beside the slide cancels");
    assert_eq!(take_result(&h.ctx), None);

    // Without an open colour menu there is nothing to hand a colour to.
    let mut other = harness();
    request(&other.ctx);
    other.run_steps(3);
    assert!(other.state().eyedropper.is_none());
}

#[test]
fn set_transparent_color_takes_the_colour_clicked_in_the_picture() {
    // Red on the left half, blue on the right.
    let pixels = (0..8).flat_map(|i| if i % 4 < 2 { [255, 0, 0, 255] } else { [0, 0, 255, 255] }).collect();
    let png = deckcraft_render::Image { width: 4, height: 2, pixels }.to_png();
    let mut session = Session::with_new();
    session.execute("slide.new", &json!({"layout": "blank"})).unwrap();
    let data = deckcraft_engine::cmd::base64_encode(&png);
    session.execute("insert.picture", &json!({"data": data, "name": "a.png", "rect": [100, 100, 400, 200]})).unwrap();
    let mut app = SlideApp::new(session, Services::default());
    app.ui.tab = "Picture Format".into();
    let mut h = Harness::builder().with_size(egui::vec2(1600.0, 1200.0)).build_ui_state(
        |ui, app: &mut SlideApp| {
            app.logic(ui.ctx());
            app.ui(ui);
        },
        app,
    );
    h.run_steps(4);
    // A colour menu opened earlier (Shape Fill, say) must not receive the colour.
    note_open_menu(&h.ctx, menu());
    h.get_by_label("Color").click();
    h.run_steps(3);
    h.get_by_label("Set Transparent Color").click();
    h.run_steps(3);
    assert!(h.state().eyedropper.is_some(), "the command arms the pointer");
    let blue_half = at(&h, 450.0, 200.0);
    click(&mut h, blue_half);
    assert!(h.state().eyedropper.is_none(), "one click ends it");
    assert!(!result_waits_for(&h.ctx, menu()), "the colour went to the picture, not to another menu");
    let doc = h.state().session.doc().unwrap();
    let clear = doc.shapes().iter().find_map(|sh| match &sh.kind {
        deckcraft_model::ShapeKind::Picture { fill } => Some(fill.adjust.clear_color),
        _ => None,
    });
    assert_eq!(clear, Some(Some(deckcraft_color::Rgba::rgb(0, 0, 255))));
}

/// The whole round trip through a real menu: Home ▸ Font Color ▸ Eyedropper, a click on the red
/// rectangle, and the selected text box's text turns red. Likewise Shape Fill on the text box.
#[test]
fn the_eyedropper_in_a_ribbon_menu_colours_the_selection() {
    let mut h = harness();
    let made = h.state_mut().session.execute("insert.textBox", &json!({"rect": [400, 300, 300, 60], "text": "Hello"})).unwrap();
    let id = u32::try_from(made["id"].as_u64().unwrap()).unwrap();
    h.state_mut().session.execute("edit.select", &json!({"ids": [id]})).unwrap();
    h.run_steps(4);
    let shape = |h: &Harness<'static, SlideApp>| {
        let d = h.state().session.doc().unwrap();
        d.doc.slides[d.selection.slide].shapes.iter().find(|s| s.id.0 == id).cloned().unwrap()
    };
    let pick = |h: &mut Harness<'static, SlideApp>, menu: &str| {
        let arrow = h.get_by_label(menu).rect().center();
        click(h, arrow);
        let eye = h.get_by_label("Eyedropper").rect().center();
        click(h, eye);
        assert!(h.state().eyedropper.is_some(), "{menu}: the Eyedropper waits for a click");
        let red = at(h, 150.0, 150.0);
        click(h, red);
        h.run_steps(4);
        assert!(h.state().eyedropper.is_none());
        assert!(h.state().status.as_ref().is_none_or(|(m, _)| !m.starts_with("Eyedropper")), "the prompt is gone once the colour is taken");
    };
    pick(&mut h, "Font Color options");
    let text = format!("{:?}", shape(&h).text.unwrap().paragraphs[0].runs[0].props.fill);
    assert!(text.contains("255, g: 0, b: 0") || text.to_uppercase().contains("FF0000"), "the text is red: {text}");
    pick(&mut h, "Shape Fill options");
    let fill = format!("{:?}", shape(&h).fill);
    assert!(fill.contains("255, g: 0, b: 0") || fill.to_uppercase().contains("FF0000"), "the fill is red: {fill}");
}

/// Font Color ▸ Eyedropper while words of a text box are selected for editing colours those words.
#[test]
fn the_eyedropper_colours_selected_text_while_editing() {
    let mut h = harness();
    let made = h.state_mut().session.execute("insert.textBox", &json!({"rect": [400, 300, 300, 60], "text": "Hello"})).unwrap();
    let id = u32::try_from(made["id"].as_u64().unwrap()).unwrap();
    h.state_mut().session.execute("text.edit", &json!({"id": id})).unwrap();
    h.state_mut().session.execute("text.selectAll", &json!({})).unwrap();
    h.run_steps(4);
    let arrow = h.get_by_label("Font Color options").rect().center();
    click(&mut h, arrow);
    let eye = h.get_by_label("Eyedropper").rect().center();
    click(&mut h, eye);
    assert!(h.state().eyedropper.is_some(), "the Eyedropper waits for a click");
    let red = at(&h, 150.0, 150.0);
    click(&mut h, red);
    h.run_steps(4);
    let d = h.state().session.doc().unwrap();
    let sh = d.doc.slides[d.selection.slide].shapes.iter().find(|s| s.id.0 == id).cloned().unwrap();
    let text = format!("{:?}", sh.text.unwrap().paragraphs[0].runs[0].props.fill);
    assert!(text.contains("255, g: 0, b: 0") || text.to_uppercase().contains("FF0000"), "the text is red: {text}");
}

/// Shape Format ▸ Text Fill and Text Outline (split menus) hand the Eyedropper's colour back too.
#[test]
fn the_eyedropper_works_in_the_shape_format_split_menus() {
    let mut h = harness();
    let made = h.state_mut().session.execute("insert.textBox", &json!({"rect": [400, 300, 300, 60], "text": "Hello"})).unwrap();
    let id = u32::try_from(made["id"].as_u64().unwrap()).unwrap();
    h.state_mut().session.execute("edit.select", &json!({"ids": [id]})).unwrap();
    h.state_mut().ui.tab = "Shape Format".into();
    h.run_steps(4);
    for menu in ["Text Fill options", "Text Outline options"] {
        let arrow = h.get_by_label(menu).rect().center();
        click(&mut h, arrow);
        let eye = h.get_by_label("Eyedropper").rect().center();
        click(&mut h, eye);
        assert!(h.state().eyedropper.is_some(), "{menu}: the Eyedropper waits for a click");
        let red = at(&h, 150.0, 150.0);
        click(&mut h, red);
        h.run_steps(4);
    }
    let d = h.state().session.doc().unwrap();
    let sh = d.doc.slides[d.selection.slide].shapes.iter().find(|s| s.id.0 == id).cloned().unwrap();
    let props = format!("{:?}", sh.text.unwrap().paragraphs[0].runs[0].props);
    assert!(props.matches("255, g: 0, b: 0").count() + props.to_uppercase().matches("FF0000").count() >= 2, "fill and outline are red: {props}");
}

/// In Slide Master view the Eyedropper takes colours from the master being edited and colours the
/// selected text there. It used to refuse ("takes colours from a slide").
#[test]
fn the_eyedropper_works_in_slide_master_view() {
    let mut h = harness();
    h.state_mut().run("view.slideMaster", json!({"master": 0})).unwrap();
    h.state_mut().session.execute("shape.insert", &json!({"preset": "rect", "rect": [100, 100, 200, 100]})).unwrap();
    h.state_mut().session.execute("shape.fill", &json!({"color": "#ff0000"})).unwrap();
    let made = h.state_mut().session.execute("insert.textBox", &json!({"rect": [400, 300, 300, 60], "text": "Hello"})).unwrap();
    let id = u32::try_from(made["id"].as_u64().unwrap()).unwrap();
    h.state_mut().session.execute("edit.select", &json!({"ids": [id]})).unwrap();
    h.state_mut().ui.tab = "Home".into();
    h.run_steps(4);
    let arrow = h.get_by_label("Font Color options").rect().center();
    click(&mut h, arrow);
    let eye = h.get_by_label("Eyedropper").rect().center();
    click(&mut h, eye);
    assert!(h.state().eyedropper.is_some(), "the Eyedropper waits for a click on the master");
    let red = at(&h, 150.0, 150.0);
    click(&mut h, red);
    h.run_steps(4);
    let d = h.state().session.doc().unwrap();
    assert_ne!(d.selection.target, deckcraft_engine::Target::Slides, "still in Slide Master view");
    let sh = d.selected_shapes().into_iter().find(|s| s.id.0 == id).cloned().expect("the text box is still selected");
    let text = format!("{:?}", sh.text.unwrap().paragraphs[0].runs[0].props.fill);
    assert!(text.contains("255, g: 0, b: 0") || text.to_uppercase().contains("FF0000"), "the text is red: {text}");
}
