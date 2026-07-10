// Nessuna finestra della console su Windows. Vale sempre (non solo in release)
// perché l'app può essere lanciata dall'autostart anche come build di debug,
// e la console nera resterebbe visibile a ogni avvio con Windows.
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    tauri_app_lib::run()
}
