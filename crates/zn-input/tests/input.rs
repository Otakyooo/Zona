use zn_input::InputState;
use winit::event::MouseButton;
use winit::keyboard::KeyCode;

#[test]
fn pressed_keys_track_until_released_and_end_frame() {
    let mut input = InputState::new();
    input.press_key(KeyCode::KeyW);
    assert!(input.is_key_down(KeyCode::KeyW));
    assert!(input.just_pressed(KeyCode::KeyW));
    input.press_key(KeyCode::KeyW);

    input.release_key(KeyCode::KeyW);
    assert!(!input.is_key_down(KeyCode::KeyW));
    assert!(input.just_released(KeyCode::KeyW));

    input.end_frame();
    assert!(!input.just_released(KeyCode::KeyW));
}

#[test]
fn orthogonal_axes() {
    let mut input = InputState::new();
    input.press_key(KeyCode::KeyD);
    input.press_key(KeyCode::KeyW);
    let axis = input.move_axis(KeyCode::KeyW, KeyCode::KeyA, KeyCode::KeyS, KeyCode::KeyD);
    assert_eq!(axis, [1.0, 1.0]);
}

#[test]
fn losing_focus_clears_stuck_keys() {
    let mut input = InputState::new();
    input.press_key(KeyCode::KeyA);
    input.set_focused(false);
    assert!(!input.focused());
    assert!(!input.is_key_down(KeyCode::KeyA));
}

#[test]
fn mouse_motion_and_buttons_accumulate_until_end_frame() {
    let mut input = InputState::new();
    input.add_mouse_motion(10.0, -5.0);
    input.add_mouse_motion(2.0, 1.0);
    assert_eq!(input.mouse_delta(), (12.0, -4.0));

    input.press_button(MouseButton::Left);
    assert!(input.is_button_down(MouseButton::Left));
    assert!(input.button_just_pressed(MouseButton::Left));

    input.end_frame();
    assert_eq!(input.mouse_delta(), (0.0, 0.0));
    assert!(!input.button_just_pressed(MouseButton::Left));
    assert!(input.is_button_down(MouseButton::Left));

    input.release_button(MouseButton::Left);
    assert!(!input.is_button_down(MouseButton::Left));
}

#[test]
fn wheel_delta_is_taken() {
    let mut input = InputState::new();
    input.add_wheel(2.0);
    input.add_wheel(0.5);
    assert_eq!(input.wheel_delta(), 2.5);
    assert_eq!(input.take_wheel_delta(), 2.5);
    assert_eq!(input.wheel_delta(), 0.0);
}
