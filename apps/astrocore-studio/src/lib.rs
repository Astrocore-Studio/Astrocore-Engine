//! Состояние и логика Astrocore Studio без окна и GPU.
//!
//! Оболочка (окно, панели egui, камера) только передаёт сюда события указателя и
//! нажатия кнопок; всё решение «что сделать с картой» — здесь и покрыто тестами.

pub mod state;

pub use state::{Pointer, Preview, StudioState, Tool};

#[cfg(test)]
mod tests;
