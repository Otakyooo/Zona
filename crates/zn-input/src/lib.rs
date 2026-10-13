use std::collections::HashSet;

use winit::event::{DeviceEvent, ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

#[derive(Clone, Debug, Default)]
pub struct InputState {
    down: HashSet<KeyCode>,
    just_pressed: HashSet<KeyCode>,
    just_released: HashSet<KeyCode>,
    buttons_down: HashSet<MouseButton>,
    buttons_just_pressed: HashSet<MouseButton>,
    buttons_just_released: HashSet<MouseButton>,
    mouse_delta: (f32, f32),
    wheel_delta: f32,
    cursor_position: Option<(f32, f32)>,
    focused: bool,
}

impl InputState {
    pub fn new() -> Self {
        Self { focused: true, ..Self::default() }
    }

    pub fn handle_event(&mut self, event: &WindowEvent) -> bool {
        match event {
            WindowEvent::KeyboardInput { event, .. } => match event.physical_key {
                PhysicalKey::Code(code) => match event.state {
                    ElementState::Pressed => {
                        if event.repeat {
                            return true;
                        }
                        self.press_key(code)
                    }
                    ElementState::Released => self.release_key(code),
                },
                PhysicalKey::Unidentified(_) => {}
            },
            WindowEvent::Focused(focused) => self.set_focused(*focused),
            WindowEvent::MouseInput { button, state, .. } => match state {
                ElementState::Pressed => self.press_button(*button),
                ElementState::Released => self.release_button(*button),
            },
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_position = Some((position.x as f32, position.y as f32));
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if let MouseScrollDelta::LineDelta(_, y) = delta {
                    self.add_wheel(*y);
                }
            }
            _ => return false,
        }
        true
    }

    pub fn handle_device_event(&mut self, event: &DeviceEvent) -> bool {
        match event {
            DeviceEvent::MouseMotion { delta } => {
                self.add_mouse_motion(delta.0 as f32, delta.1 as f32);
            }
            DeviceEvent::MouseWheel { delta } => {
                if let MouseScrollDelta::LineDelta(_, y) = delta {
                    self.add_wheel(*y);
                }
            }
            DeviceEvent::Key(raw) => match raw.physical_key {
                PhysicalKey::Code(code) => match raw.state {
                    ElementState::Pressed => self.press_key(code),
                    ElementState::Released => self.release_key(code),
                },
                PhysicalKey::Unidentified(_) => {}
            },
            _ => return false,
        }
        true
    }

    pub fn press_key(&mut self, key: KeyCode) {
        if self.down.insert(key) {
            self.just_pressed.insert(key);
        }
    }

    pub fn release_key(&mut self, key: KeyCode) {
        if self.down.remove(&key) {
            self.just_released.insert(key);
        }
    }

    pub fn press_button(&mut self, button: MouseButton) {
        if self.buttons_down.insert(button) {
            self.buttons_just_pressed.insert(button);
        }
    }

    pub fn release_button(&mut self, button: MouseButton) {
        if self.buttons_down.remove(&button) {
            self.buttons_just_released.insert(button);
        }
    }

    pub fn add_mouse_motion(&mut self, dx: f32, dy: f32) {
        self.mouse_delta.0 += dx;
        self.mouse_delta.1 += dy;
    }

    pub fn add_wheel(&mut self, delta: f32) {
        self.wheel_delta += delta;
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        if !focused {
            self.clear_down();
        }
    }

    pub fn is_key_down(&self, key: KeyCode) -> bool {
        self.down.contains(&key)
    }

    pub fn just_pressed(&self, key: KeyCode) -> bool {
        self.just_pressed.contains(&key)
    }

    pub fn just_released(&self, key: KeyCode) -> bool {
        self.just_released.contains(&key)
    }

    pub fn is_button_down(&self, button: MouseButton) -> bool {
        self.buttons_down.contains(&button)
    }

    pub fn button_just_pressed(&self, button: MouseButton) -> bool {
        self.buttons_just_pressed.contains(&button)
    }

    pub fn button_just_released(&self, button: MouseButton) -> bool {
        self.buttons_just_released.contains(&button)
    }

    pub fn mouse_delta(&self) -> (f32, f32) {
        self.mouse_delta
    }

    pub fn take_mouse_delta(&mut self) -> (f32, f32) {
        std::mem::take(&mut self.mouse_delta)
    }

    pub fn wheel_delta(&self) -> f32 {
        self.wheel_delta
    }

    pub fn take_wheel_delta(&mut self) -> f32 {
        std::mem::take(&mut self.wheel_delta)
    }

    pub fn cursor_position(&self) -> Option<(f32, f32)> {
        self.cursor_position
    }

    pub fn focused(&self) -> bool {
        self.focused
    }

    pub fn axis_1d(&self, negative: KeyCode, positive: KeyCode) -> f32 {
        let negative = self.is_key_down(negative) as i32 as f32;
        let positive = self.is_key_down(positive) as i32 as f32;
        negative - positive
    }

    pub fn move_axis(&self, forward: KeyCode, left: KeyCode, back: KeyCode, right: KeyCode) -> [f32; 2] {
        let right_axis = self.is_key_down(right) as i32 as f32 - self.is_key_down(left) as i32 as f32;
        let forward_axis = self.is_key_down(forward) as i32 as f32 - self.is_key_down(back) as i32 as f32;
        [right_axis, forward_axis]
    }

    pub fn end_frame(&mut self) {
        self.just_pressed.clear();
        self.just_released.clear();
        self.buttons_just_pressed.clear();
        self.buttons_just_released.clear();
        self.wheel_delta = 0.0;
        self.mouse_delta = (0.0, 0.0);
    }

    pub fn clear_down(&mut self) {
        self.down.clear();
        self.buttons_down.clear();
    }
}
