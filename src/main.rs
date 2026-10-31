#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use minifb::{Key, KeyRepeat, Scale, Window, WindowOptions};
use std::collections::VecDeque;
use std::env;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

// ============================================================================
// RESOLUÇÃO INTERNA DO EMULADOR E HUB (WIDESCREEN 16:9 NATIVO PARA UI ARCADE)
// ============================================================================
pub const CANVAS_WIDTH: usize = 512;
pub const CANVAS_HEIGHT: usize = 288;
pub const NES_WIDTH: usize = 256;
pub const NES_HEIGHT: usize = 240;

// ============================================================================
// SISTEMA DE DOWNLOAD E CACHE DE CAPAS OFICIAIS (LIBRETRO BOX ART DATABASE)
// ============================================================================
fn get_cover_cache_dir() -> PathBuf {
    let dir = PathBuf::from(".covers");
    let _ = fs::create_dir_all(&dir);
    dir
}

fn sanitize_rom_name_for_scraping(name: &str) -> String {
    let mut clean = String::new();
    let mut in_paren = false;
    let mut in_bracket = false;

    for ch in name.chars() {
        if ch == '(' { in_paren = true; continue; }
        if ch == ')' { in_paren = false; continue; }
        if ch == '[' { in_bracket = true; continue; }
        if ch == ']' { in_bracket = false; continue; }

        // Remove pontos e caracteres que quebram busca no Libretro
        if !in_paren && !in_bracket {
            if ch == '.' || ch == ',' || ch == '!' || ch == ':' {
                clean.push(' ');
            } else {
                clean.push(ch);
            }
        }
    }
    // Remove espaços duplos e trim
    clean.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ============================================================================
// DOWNLOAD ROBUSTO DE CAPAS OFICIAIS (MULTI-FONTE LIBRETRO)
// ============================================================================
fn download_or_load_boxart(rom_name: &str, target_w: usize, target_h: usize) -> Option<Vec<u32>> {
    let clean_title = sanitize_rom_name_for_scraping(rom_name);
    let cache_dir = get_cover_cache_dir();
    let cache_file = cache_dir.join(format!("{}.raw", clean_title.replace(' ', "_")));

    // 1. CARREGA DO CACHE LOCAL (.covers/*.raw)
    if let Ok(bytes) = fs::read(&cache_file) {
        if bytes.len() == target_w * target_h * 4 {
            let mut raw_pixels = vec![0u32; target_w * target_h];
            for (i, chunk) in bytes.chunks_exact(4).enumerate() {
                raw_pixels[i] = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            }
            return Some(raw_pixels);
        }
    }

    // 2. TENTA CAPA LOCAL CASO EXISTA (.png/.jpg com nome da ROM)
    let img_extensions = ["png", "jpg", "jpeg", "webp"];
    for ext in &img_extensions {
        let local_img = PathBuf::from(format!("{}.{}", rom_name, ext));
        if local_img.exists() {
            if let Ok(img) = image::open(&local_img) {
                let resized = img.resize_exact(target_w as u32, target_h as u32, image::imageops::FilterType::Lanczos3);
                let rgba = resized.to_rgba8();
                let mut pixels = vec![0u32; target_w * target_h];
                for (i, p) in rgba.pixels().enumerate() {
                    pixels[i] = ((p[0] as u32) << 16) | ((p[1] as u32) << 8) | (p[2] as u32);
                }
                return Some(pixels);
            }
        }
    }

    // 3. VARIAÇÕES DE NOMES POSSÍVEIS DO JOGO
    let mut candidates = vec![
        format!("{} (USA)", clean_title),
        clean_title.clone(),
        rom_name.trim().to_string(),
    ];

    // Tratamento especial para números romanos comuns no NES (ex: Mega Man 5 -> Mega Man V)
    if clean_title.contains(" 5") {
        candidates.push(clean_title.replace(" 5", " V (USA)"));
    } else if clean_title.contains(" 4") {
        candidates.push(clean_title.replace(" 4", " IV (USA)"));
    } else if clean_title.contains(" 3") {
        candidates.push(clean_title.replace(" 3", " III (USA)"));
    } else if clean_title.contains(" 2") {
        candidates.push(clean_title.replace(" 2", " II (USA)"));
    }

    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_millis(3500)) // 3.5 segundos para conexões lentas
        .build();

    for title in &candidates {
        let encoded = urlencoding_simple(title);

        // Testa nos dois repositórios oficiais mais rápidos
        let urls = [
            format!("https://raw.githubusercontent.com/libretro-thumbnails/Nintendo_-_Nintendo_Entertainment_System/master/Named_Boxarts/{}.png", encoded),
            format!("http://thumbnails.libretro.com/Nintendo%20-%20Nintendo%20Entertainment%20System/Named_Boxarts/{}.png", encoded),
        ];

        for url in &urls {
            if let Ok(response) = agent.get(url).call() {
                let mut img_bytes = Vec::new();
                if response.into_reader().read_to_end(&mut img_bytes).is_ok() {
                    if let Ok(img) = image::load_from_memory(&img_bytes) {
                        let resized = img.resize_exact(target_w as u32, target_h as u32, image::imageops::FilterType::Lanczos3);
                        let rgba = resized.to_rgba8();
                        let mut pixels = vec![0u32; target_w * target_h];
                        let mut byte_cache = Vec::with_capacity(target_w * target_h * 4);

                        for (i, p) in rgba.pixels().enumerate() {
                            let col = ((p[0] as u32) << 16) | ((p[1] as u32) << 8) | (p[2] as u32);
                            pixels[i] = col;
                            byte_cache.extend_from_slice(&col.to_le_bytes());
                        }

                        let _ = fs::write(cache_file, byte_cache);
                        return Some(pixels);
                    }
                }
            }
        }
    }

    None
}

fn urlencoding_simple(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

// ============================================================================
// CONFIGURAÇÕES AVANÇADAS DO EMULADOR (MENU MODERNO POR ABAS)
// ============================================================================
// ============================================================================
// MODOS DE EXIBIÇÃO DE VÍDEO
// ============================================================================
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum DisplayMode {
    Windowed = 0,   // Janela com bordas e título
    Borderless = 1, // Janela sem bordas
    Fullscreen = 2, // Tela cheia total
}

impl DisplayMode {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Borderless,
            2 => Self::Fullscreen,
            _ => Self::Windowed,
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Windowed => Self::Borderless,
            Self::Borderless => Self::Fullscreen,
            Self::Fullscreen => Self::Windowed,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::Windowed => Self::Fullscreen,
            Self::Borderless => Self::Windowed,
            Self::Fullscreen => Self::Borderless,
        }
    }
}

#[derive(Clone)]
pub struct EmulatorSettings {
    // VÍDEO
    pub aspect_mode: u8, // 0 = 4:3, 1 = Ambient, 2 = Zoom, 3 = Esticado
    pub crt_scanlines: bool,
    pub sprite_limit: bool,
    pub display_mode: DisplayMode, // <-- NOVO: Modo de Tela

    // ÁUDIO
    pub volume: f32,
    pub mute_all: bool,

    // SISTEMA
    pub active_save_slot: usize,
    pub turbo_multiplier: usize,
}

impl Default for EmulatorSettings {
    fn default() -> Self {
        Self {
            aspect_mode: 1,
            crt_scanlines: false,
            sprite_limit: true,
            display_mode: DisplayMode::Windowed,
            volume: 0.8,
            mute_all: false,
            active_save_slot: 1,
            turbo_multiplier: 2,
        }
    }
}

impl EmulatorSettings {
    fn load() -> Self {
        if let Ok(bytes) = fs::read(".emulator_settings.bin") {
            if bytes.len() >= 8 {
                let aspect_mode = if bytes.len() >= 9 && bytes[8] == 2 {
                    bytes[1] % 4
                } else if bytes[1] != 0 {
                    0 // Compatibilidade com o formato antigo: true significava 4:3
                } else {
                    3 // Compatibilidade com o formato antigo: false significava 16:9 esticado
                };
                return Self {
                    display_mode: DisplayMode::from_u8(bytes[0]),
                    aspect_mode,
                    crt_scanlines: bytes[2] != 0,
                    sprite_limit: bytes[3] != 0,
                    volume: (bytes[4] as f32 / 100.0).clamp(0.0, 1.0),
                    mute_all: bytes[5] != 0,
                    active_save_slot: (bytes[6] as usize).clamp(1, 3),
                    turbo_multiplier: (bytes[7] as usize).clamp(2, 4),
                };
            }
        }
        Self::default()
    }

    fn save(&self) {
        let bytes = [
            self.display_mode as u8,
            self.aspect_mode % 4,
            self.crt_scanlines as u8,
            self.sprite_limit as u8,
            (self.volume * 100.0).round() as u8,
            self.mute_all as u8,
            self.active_save_slot as u8,
            self.turbo_multiplier as u8,
            2, // versão do formato de configurações
        ];
        let _ = fs::write(".emulator_settings.bin", bytes);
    }

    fn aspect_label(&self) -> &'static str {
        match self.aspect_mode {
            0 => "< 4:3 CLASSICO >",
            1 => "< 16:9 AMBIENT >",
            2 => "< 16:9 ZOOM >",
            _ => "< 16:9 ESTICADO >",
        }
    }
}

// ============================================================================
// INTEGRAÇÃO NATIVA WINDOWS (XINPUT GAMEPAD + FILE DIALOG COMMDLG32)
// ============================================================================
#[repr(C)]
struct XInputGamepad {
    w_buttons: u16,
    b_left_trigger: u8,
    b_right_trigger: u8,
    s_thumb_lx: i16,
    s_thumb_ly: i16,
    s_thumb_rx: i16,
    s_thumb_ry: i16,
}

#[repr(C)]
struct XInputState {
    dw_packet_number: u32,
    gamepad: XInputGamepad,
}

#[cfg(target_os = "windows")]
type XInputGetStateFn = unsafe extern "system" fn(dw_user_index: u32, p_state: *mut XInputState) -> u32;

#[cfg(target_os = "windows")]
fn get_xinput_fn() -> Option<XInputGetStateFn> {
    use std::sync::OnceLock;
    static XINPUT_FN: OnceLock<Option<XInputGetStateFn>> = OnceLock::new();

    *XINPUT_FN.get_or_init(|| unsafe {
        extern "system" {
            fn LoadLibraryA(lpLibFileName: *const u8) -> *mut std::ffi::c_void;
            fn GetProcAddress(hModule: *mut std::ffi::c_void, lpProcName: *const u8) -> *mut std::ffi::c_void;
        }

        let dlls = [b"xinput1_4.dll\0".as_ptr(), b"xinput1_3.dll\0".as_ptr(), b"xinput9_1_0.dll\0".as_ptr()];
        for &dll in &dlls {
            let handle = LoadLibraryA(dll);
            if !handle.is_null() {
                let proc = GetProcAddress(handle, b"XInputGetState\0".as_ptr());
                if !proc.is_null() {
                    let func: XInputGetStateFn = std::mem::transmute(proc);
                    return Some(func);
                }
            }
        }
        None
    })
}

// ============================================================================
// ENTRADA NATIVA XINPUT (XBOX GAMEPAD COM BUMPERS E GATILHOS)
// ============================================================================
pub struct XboxInputState {
    pub connected: bool,
    pub nes_buttons: u8,
    pub lb_held: bool,
    pub rb_held: bool,
    pub lt_held: bool,
    pub rt_held: bool,
    pub menu_btn_held: bool,
    pub menu_btn_pressed: bool,
    pub y_pressed: bool,
    pub x_pressed: bool,
    pub b_pressed: bool,
}

fn poll_xbox_controller() -> XboxInputState {
    #[cfg(target_os = "windows")]
    if let Some(xinput_get_state) = get_xinput_fn() {
        unsafe {
            let mut state: XInputState = std::mem::zeroed();
            for pad_idx in 0..4 {
                if xinput_get_state(pad_idx, &mut state) == 0 {
                    let b = state.gamepad.w_buttons;
                    let lx = state.gamepad.s_thumb_lx;
                    let ly = state.gamepad.s_thumb_ly;

                    let mut nes_pad = 0u8;
                    if (b & 0x1000) != 0 { nes_pad |= 1 << 0; } // A -> A
                    if (b & 0x2000) != 0 { nes_pad |= 1 << 1; } // B -> B
                    if (b & 0x0020) != 0 { nes_pad |= 1 << 2; } // Back -> Select
                    if (b & 0x0010) != 0 { nes_pad |= 1 << 3; } // Start -> Start

                    if (b & 0x0001) != 0 || ly > 12000  { nes_pad |= 1 << 4; } // Up
                    if (b & 0x0002) != 0 || ly < -12000 { nes_pad |= 1 << 5; } // Down
                    if (b & 0x0004) != 0 || lx < -12000 { nes_pad |= 1 << 6; } // Left
                    if (b & 0x0008) != 0 || lx > 12000  { nes_pad |= 1 << 7; } // Right

                    let lb = (b & 0x0100) != 0;
                    let rb = (b & 0x0200) != 0;
                    let lt = state.gamepad.b_left_trigger > 60;
                    let rt = state.gamepad.b_right_trigger > 60;
                    let menu = (b & 0x0020) != 0 && (b & 0x0010) != 0;
                    let y_btn = (b & 0x8000) != 0;
                    let x_btn = (b & 0x4000) != 0;
                    let b_btn = (b & 0x2000) != 0;

                    return XboxInputState {
                        connected: true,
                        nes_buttons: nes_pad,
                        lb_held: lb,
                        rb_held: rb,
                        lt_held: lt,
                        rt_held: rt,
                        menu_btn_held: menu,
                        menu_btn_pressed: menu,
                        y_pressed: y_btn,
                        x_pressed: x_btn,
                        b_pressed: b_btn,
                    };
                }
            }
        }
    }
    XboxInputState {
        connected: false,
        nes_buttons: 0,
        lb_held: false,
        rb_held: false,
        lt_held: false,
        rt_held: false,
        menu_btn_held: false,
        menu_btn_pressed: false,
        y_pressed: false,
        x_pressed: false,
        b_pressed: false,
    }
}

#[cfg(target_os = "windows")]
fn apply_window_mode(window: &minifb::Window, mode: DisplayMode) {
    let hwnd_ptr = window.get_window_handle();
    if hwnd_ptr.is_null() {
        return;
    }

    unsafe {
        extern "system" {
            fn SetWindowLongPtrA(hwnd: *mut std::ffi::c_void, index: i32, new_long: isize) -> isize;
            fn SetWindowPos(
                hwnd: *mut std::ffi::c_void,
                insert_after: *mut std::ffi::c_void,
                x: i32,
                y: i32,
                cx: i32,
                cy: i32,
                flags: u32,
            ) -> i32;
            fn GetSystemMetrics(index: i32) -> i32;
            fn ShowWindow(hwnd: *mut std::ffi::c_void, command: i32) -> i32;
        }

        let screen_w = GetSystemMetrics(0); // SM_CXSCREEN
        let screen_h = GetSystemMetrics(1); // SM_CYSCREEN

        const GWL_STYLE: i32 = -16;
        const WS_OVERLAPPEDWINDOW: isize = 0x00CF0000;
        const WS_POPUP: isize = 0x80000000;
        const WS_VISIBLE: isize = 0x10000000;
        const SWP_FRAMECHANGED: u32 = 0x0020;
        const SWP_SHOWWINDOW: u32 = 0x0040;
        const SWP_NOZORDER: u32 = 0x0004;
        const SW_RESTORE: i32 = 9;
        const HWND_TOP: *mut std::ffi::c_void = std::ptr::null_mut();
        const HWND_TOPMOST: *mut std::ffi::c_void = usize::MAX as *mut std::ffi::c_void;

        match mode {
            DisplayMode::Windowed => {
                ShowWindow(hwnd_ptr, SW_RESTORE);
                SetWindowLongPtrA(hwnd_ptr, GWL_STYLE, WS_OVERLAPPEDWINDOW | WS_VISIBLE);
                let win_w = 1024;
                let win_h = 576;
                let win_x = ((screen_w - win_w) / 2).max(0);
                let win_y = ((screen_h - win_h) / 2).max(0);
                SetWindowPos(
                    hwnd_ptr,
                    HWND_TOP,
                    win_x,
                    win_y,
                    win_w,
                    win_h,
                    SWP_FRAMECHANGED | SWP_SHOWWINDOW | SWP_NOZORDER,
                );
            }
            DisplayMode::Borderless => {
                ShowWindow(hwnd_ptr, SW_RESTORE);
                SetWindowLongPtrA(hwnd_ptr, GWL_STYLE, WS_POPUP | WS_VISIBLE);
                SetWindowPos(
                    hwnd_ptr,
                    HWND_TOP,
                    0,
                    0,
                    screen_w,
                    screen_h,
                    SWP_FRAMECHANGED | SWP_SHOWWINDOW | SWP_NOZORDER,
                );
            }
            DisplayMode::Fullscreen => {
                SetWindowLongPtrA(hwnd_ptr, GWL_STYLE, WS_POPUP | WS_VISIBLE);
                SetWindowPos(
                    hwnd_ptr,
                    HWND_TOPMOST,
                    0,
                    0,
                    screen_w,
                    screen_h,
                    SWP_FRAMECHANGED | SWP_SHOWWINDOW | SWP_NOZORDER,
                );
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn apply_window_mode(_window: &minifb::Window, _mode: DisplayMode) {}

// Dialog nativo do Windows para selecionar pasta
#[repr(C)]
struct BrowseInfoW {
    hwnd_owner: usize,
    pidl_root: usize,
    psz_display_name: *mut u16,
    lpsz_title: *const u16,
    ul_flags: u32,
    lpfn: usize,
    l_param: usize,
    i_image: i32,
}

#[cfg(target_os = "windows")]
type SHBrowseForFolderWFn = unsafe extern "system" fn(lpbi: *mut BrowseInfoW) -> usize;
#[cfg(target_os = "windows")]
type SHGetPathFromIDListWFn = unsafe extern "system" fn(pidl: usize, psz_path: *mut u16) -> i32;
#[cfg(target_os = "windows")]
type CoTaskMemFreeFn = unsafe extern "system" fn(pv: usize);

fn open_folder_dialog() -> Option<String> {
    #[cfg(target_os = "windows")]
    unsafe {
        extern "system" {
            fn LoadLibraryA(lpLibFileName: *const u8) -> *mut std::ffi::c_void;
            fn GetProcAddress(hModule: *mut std::ffi::c_void, lpProcName: *const u8) -> *mut std::ffi::c_void;
        }

        let shell32 = LoadLibraryA(b"shell32.dll\0".as_ptr());
        let ole32 = LoadLibraryA(b"ole32.dll\0".as_ptr());
        if shell32.is_null() || ole32.is_null() {
            return None;
        }

        let sh_browse: SHBrowseForFolderWFn = std::mem::transmute(GetProcAddress(shell32, b"SHBrowseForFolderW\0".as_ptr()));
        let sh_path: SHGetPathFromIDListWFn = std::mem::transmute(GetProcAddress(shell32, b"SHGetPathFromIDListW\0".as_ptr()));
        let co_free: CoTaskMemFreeFn = std::mem::transmute(GetProcAddress(ole32, b"CoTaskMemFree\0".as_ptr()));

        let title: Vec<u16> = "Selecione a Pasta contendo suas ROMs (.nes)\0".encode_utf16().collect();
        let mut display_name = [0u16; 260];
        let mut bi: BrowseInfoW = std::mem::zeroed();
        bi.lpsz_title = title.as_ptr();
        bi.psz_display_name = display_name.as_mut_ptr();
        bi.ul_flags = 0x00000001 | 0x00000040;

        let pidl = sh_browse(&mut bi);
        if pidl != 0 {
            let mut path_buf = [0u16; 260];
            let success = sh_path(pidl, path_buf.as_mut_ptr());
            co_free(pidl);
            if success != 0 {
                let len = path_buf.iter().position(|&c| c == 0).unwrap_or(0);
                return Some(String::from_utf16_lossy(&path_buf[..len]));
            }
        }
    }
    None
}

// ============================================================================
// PALETA DE CORES OFICIAL DA PPU 2C02 (64 Cores RGB)
// ============================================================================
const SYSTEM_PALETTE: [u32; 64] = [
    0x666666, 0x002A88, 0x1412A7, 0x3B00A4, 0x5C007E, 0x6E0040, 0x6C0600, 0x561D00,
    0x333500, 0x0B4800, 0x005200, 0x004F08, 0x00404D, 0x000000, 0x000000, 0x000000,
    0xADADAD, 0x155FD9, 0x4240FF, 0x7527FE, 0xA01ACC, 0xB71E7B, 0xB53120, 0x994E00,
    0x6B6D00, 0x388700, 0x0C9300, 0x008F32, 0x007C8D, 0x000000, 0x000000, 0x000000,
    0xFFFEFF, 0x64B0FF, 0x9290FF, 0xC676FF, 0xF36AFF, 0xFE6ECC, 0xFE8170, 0xEA9E22,
    0xBCBE00, 0x88D800, 0x5CE430, 0x45E082, 0x48CDDE, 0x4F4F4F, 0x000000, 0x000000,
    0xFFFEFF, 0xC0DFFF, 0xD3D2FF, 0xE8C8FF, 0xFBC2FF, 0xFEC4EA, 0xFECCC5, 0xF7D8A5,
    0xE4E594, 0xCFEF96, 0xBDF4AB, 0xB3F3CC, 0xB5EBF2, 0xB8B8B8, 0x000000, 0x000000,
];

// ============================================================================
// APU 2A03 (AUDIO PROCESSING UNIT - 5 CANAIS DE SOM)
// ============================================================================
const LENGTH_TABLE: [u8; 32] = [
    10, 254, 20, 2, 40, 4, 80, 6, 160, 8, 60, 10, 14, 12, 26, 14,
    12, 16, 24, 18, 48, 20, 96, 22, 192, 24, 72, 26, 16, 28, 32, 30,
];

const DUTY_TABLE: [[u8; 8]; 4] = [
    [0, 1, 0, 0, 0, 0, 0, 0],
    [0, 1, 1, 0, 0, 0, 0, 0],
    [0, 1, 1, 1, 1, 0, 0, 0],
    [1, 0, 0, 1, 1, 1, 1, 1],
];

const TRIANGLE_SEQ: [u8; 32] = [
    15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0,
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
];

const NOISE_PERIOD_TABLE: [u16; 16] = [
    4, 8, 16, 32, 64, 96, 128, 160, 202, 254, 380, 508, 762, 1016, 2034, 4068,
];

#[derive(Default, Clone)]
struct Envelope {
    start: bool,
    loop_flag: bool,
    constant_volume: bool,
    volume: u8,
    decay_level: u8,
    divider: u8,
}

impl Envelope {
    fn clock(&mut self) {
        if !self.start {
            if self.divider == 0 {
                self.divider = self.volume;
                if self.decay_level > 0 {
                    self.decay_level -= 1;
                } else if self.loop_flag {
                    self.decay_level = 15;
                }
            } else {
                self.divider -= 1;
            }
        } else {
            self.start = false;
            self.decay_level = 15;
            self.divider = self.volume;
        }
    }

    fn output(&self) -> u8 {
        if self.constant_volume { self.volume } else { self.decay_level }
    }
}

#[derive(Clone)]
pub struct PulseChannel {
    enabled: bool,
    is_channel_1: bool,
    duty: u8,
    duty_pos: u8,
    length_halt: bool,
    length_counter: u8,
    env: Envelope,
    sweep_enabled: bool,
    sweep_period: u8,
    sweep_negate: bool,
    sweep_shift: u8,
    sweep_reload: bool,
    sweep_divider: u8,
    timer_period: u16,
    timer_value: u16,
}

impl PulseChannel {
    fn new(is_channel_1: bool) -> Self {
        Self {
            enabled: false,
            is_channel_1,
            duty: 0,
            duty_pos: 0,
            length_halt: false,
            length_counter: 0,
            env: Envelope::default(),
            sweep_enabled: false,
            sweep_period: 0,
            sweep_negate: false,
            sweep_shift: 0,
            sweep_reload: false,
            sweep_divider: 0,
            timer_period: 0,
            timer_value: 0,
        }
    }

    fn clock_timer(&mut self) {
        if self.timer_value == 0 {
            self.timer_value = self.timer_period;
            self.duty_pos = (self.duty_pos + 1) & 7;
        } else {
            self.timer_value -= 1;
        }
    }

    fn clock_length(&mut self) {
        if !self.length_halt && self.length_counter > 0 {
            self.length_counter -= 1;
        }
    }

    fn clock_sweep(&mut self) {
        let change = self.timer_period >> self.sweep_shift;
        let target = if self.sweep_negate {
            let sub = change + if self.is_channel_1 { 1 } else { 0 };
            self.timer_period.saturating_sub(sub)
        } else {
            self.timer_period + change
        };
        let muted = self.timer_period < 8 || target > 0x7FF;

        if self.sweep_divider == 0 && self.sweep_enabled && !muted && self.sweep_shift > 0 {
            self.timer_period = target;
        }
        if self.sweep_divider == 0 || self.sweep_reload {
            self.sweep_divider = self.sweep_period;
            self.sweep_reload = false;
        } else {
            self.sweep_divider -= 1;
        }
    }

    fn output(&self) -> u8 {
        if !self.enabled
            || self.length_counter == 0
            || self.timer_period < 8
            || self.timer_period > 0x7FF
            || DUTY_TABLE[self.duty as usize][self.duty_pos as usize] == 0
        {
            0
        } else {
            self.env.output()
        }
    }
}

#[derive(Default, Clone)]
pub struct TriangleChannel {
    enabled: bool,
    control_flag: bool,
    linear_reload_value: u8,
    linear_counter: u8,
    linear_reload: bool,
    length_counter: u8,
    timer_period: u16,
    timer_value: u16,
    seq_pos: u8,
}

impl TriangleChannel {
    fn clock_timer(&mut self) {
        if self.timer_value == 0 {
            self.timer_value = self.timer_period;
            if self.length_counter > 0 && self.linear_counter > 0 && self.timer_period >= 2 {
                self.seq_pos = (self.seq_pos + 1) & 31;
            }
        } else {
            self.timer_value -= 1;
        }
    }

    fn clock_linear(&mut self) {
        if self.linear_reload {
            self.linear_counter = self.linear_reload_value;
        } else if self.linear_counter > 0 {
            self.linear_counter -= 1;
        }
        if !self.control_flag {
            self.linear_reload = false;
        }
    }

    fn clock_length(&mut self) {
        if !self.control_flag && self.length_counter > 0 {
            self.length_counter -= 1;
        }
    }

    fn output(&self) -> u8 {
        if !self.enabled { 0 } else { TRIANGLE_SEQ[self.seq_pos as usize] }
    }
}

#[derive(Clone)]
pub struct NoiseChannel {
    enabled: bool,
    length_halt: bool,
    length_counter: u8,
    env: Envelope,
    mode: bool,
    timer_period: u16,
    timer_value: u16,
    shift_register: u16,
}

impl NoiseChannel {
    fn new() -> Self {
        Self {
            enabled: false,
            length_halt: false,
            length_counter: 0,
            env: Envelope::default(),
            mode: false,
            timer_period: 4,
            timer_value: 0,
            shift_register: 1,
        }
    }

    fn clock_timer(&mut self) {
        if self.timer_value == 0 {
            self.timer_value = self.timer_period;
            let bit0 = self.shift_register & 1;
            let other_bit = if self.mode {
                (self.shift_register >> 6) & 1
            } else {
                (self.shift_register >> 1) & 1
            };
            let feedback = bit0 ^ other_bit;
            self.shift_register = (self.shift_register >> 1) | (feedback << 14);
        } else {
            self.timer_value -= 1;
        }
    }

    fn clock_length(&mut self) {
        if !self.length_halt && self.length_counter > 0 {
            self.length_counter -= 1;
        }
    }

    fn output(&self) -> u8 {
        if !self.enabled || self.length_counter == 0 || (self.shift_register & 1) != 0 {
            0
        } else {
            self.env.output()
        }
    }
}

// ============================================================================
// CANAL DMC (DELTA MODULATION CHANNEL - AMOSTRAS PCM DE 1-BIT)
// ============================================================================
const DMC_PERIOD_TABLE: [u16; 16] = [
    428, 380, 340, 320, 286, 254, 226, 214, 190, 160, 142, 128, 106, 84, 72, 54,
];

#[derive(Clone)]
pub struct DmcChannel {
    pub enabled: bool,
    pub irq_enabled: bool,
    pub loop_flag: bool,
    pub irq_pending: bool,
    pub timer_period: u16,
    pub timer_value: u16,
    pub output_level: u8, // 0 a 127
    pub sample_address: u16,
    pub sample_length: u16,
    pub current_address: u16,
    pub bytes_remaining: u16,
    pub sample_buffer: Option<u8>,
    pub shift_register: u8,
    pub bits_remaining: u8,
    pub silence: bool,
    pub dma_read_addr: Option<u16>, // Solicita leitura direta de byte na CPU/Bus
}

impl DmcChannel {
    pub fn new() -> Self {
        Self {
            enabled: false,
            irq_enabled: false,
            loop_flag: false,
            irq_pending: false,
            timer_period: DMC_PERIOD_TABLE[0],
            timer_value: DMC_PERIOD_TABLE[0],
            output_level: 0,
            sample_address: 0xC000,
            sample_length: 1,
            current_address: 0xC000,
            bytes_remaining: 0,
            sample_buffer: None,
            shift_register: 0,
            bits_remaining: 8,
            silence: true,
            dma_read_addr: None,
        }
    }

    pub fn restart(&mut self) {
        self.current_address = self.sample_address;
        self.bytes_remaining = self.sample_length;
        self.bits_remaining = 8;
    }

    pub fn clock_timer(&mut self) {
        // Pré-carrega o próximo byte enquanto os bits atuais ainda estão tocando.
        if self.sample_buffer.is_none() && self.bytes_remaining > 0 && self.dma_read_addr.is_none() {
            self.dma_read_addr = Some(self.current_address);
            self.current_address = if self.current_address == 0xFFFF { 0x8000 } else { self.current_address + 1 };
            self.bytes_remaining -= 1;
        }
        if self.timer_value == 0 {
            self.timer_value = self.timer_period;
            self.clock_shifter();
        } else {
            self.timer_value -= 1;
        }
    }

    fn clock_shifter(&mut self) {
        if !self.silence {
            if (self.shift_register & 1) != 0 {
                if self.output_level <= 125 {
                    self.output_level += 2;
                }
            } else if self.output_level >= 2 {
                self.output_level -= 2;
            }
            self.shift_register >>= 1;
        }

        self.bits_remaining = self.bits_remaining.saturating_sub(1);
        if self.bits_remaining == 0 {
            self.bits_remaining = 8;
            if let Some(byte) = self.sample_buffer.take() {
                self.silence = false;
                self.shift_register = byte;
                // IRQ/loop só são acionados quando o último byte chega ao shifter.
                if self.bytes_remaining == 0 {
                    if self.loop_flag {
                        self.restart();
                    } else if self.irq_enabled {
                        self.irq_pending = true;
                    }
                }
            } else {
                self.silence = true;
            }
        }
    }

    pub fn output(&self) -> u8 {
        self.output_level
    }
}

#[derive(Clone)]
pub struct Apu {
    pulse1: PulseChannel,
    pulse2: PulseChannel,
    triangle: TriangleChannel,
    noise: NoiseChannel,
    pub dmc: DmcChannel,
    frame_counter_mode: u8,
    frame_step: usize,
    cpu_cycle_count: usize,
    sample_acc: f64,
    sample_rate: f64,
    high_pass_cap: f32,
    prev_raw_sample: f32,
    pub master_volume: f32,
    pub audio_buffer: Arc<Mutex<VecDeque<f32>>>,
}

impl Apu {
    pub fn new(sample_rate: f64, audio_buffer: Arc<Mutex<VecDeque<f32>>>) -> Self {
        Self {
            pulse1: PulseChannel::new(true),
            pulse2: PulseChannel::new(false),
            triangle: TriangleChannel::default(),
            noise: NoiseChannel::new(),
            dmc: DmcChannel::new(),
            frame_counter_mode: 0,
            frame_step: 0,
            cpu_cycle_count: 0,
            sample_acc: 0.0,
            sample_rate,
            high_pass_cap: 0.0,
            prev_raw_sample: 0.0,
            master_volume: 0.8,
            audio_buffer,
        }
    }

    pub fn cpu_write(&mut self, addr: u16, data: u8) {
        match addr {
            0x4000 => {
                self.pulse1.duty = (data >> 6) & 3;
                self.pulse1.length_halt = (data & 0x20) != 0;
                self.pulse1.env.loop_flag = self.pulse1.length_halt;
                self.pulse1.env.constant_volume = (data & 0x10) != 0;
                self.pulse1.env.volume = data & 0x0F;
            }
            0x4001 => {
                self.pulse1.sweep_enabled = (data & 0x80) != 0;
                self.pulse1.sweep_period = (data >> 4) & 7;
                self.pulse1.sweep_negate = (data & 0x08) != 0;
                self.pulse1.sweep_shift = data & 7;
                self.pulse1.sweep_reload = true;
            }
            0x4002 => {
                self.pulse1.timer_period = (self.pulse1.timer_period & 0x0700) | (data as u16);
            }
            0x4003 => {
                self.pulse1.timer_period =
                    (self.pulse1.timer_period & 0x00FF) | (((data & 7) as u16) << 8);
                if self.pulse1.enabled {
                    self.pulse1.length_counter = LENGTH_TABLE[(data >> 3) as usize];
                }
                self.pulse1.duty_pos = 0;
                self.pulse1.env.start = true;
            }
            0x4004 => {
                self.pulse2.duty = (data >> 6) & 3;
                self.pulse2.length_halt = (data & 0x20) != 0;
                self.pulse2.env.loop_flag = self.pulse2.length_halt;
                self.pulse2.env.constant_volume = (data & 0x10) != 0;
                self.pulse2.env.volume = data & 0x0F;
            }
            0x4005 => {
                self.pulse2.sweep_enabled = (data & 0x80) != 0;
                self.pulse2.sweep_period = (data >> 4) & 7;
                self.pulse2.sweep_negate = (data & 0x08) != 0;
                self.pulse2.sweep_shift = data & 7;
                self.pulse2.sweep_reload = true;
            }
            0x4006 => {
                self.pulse2.timer_period = (self.pulse2.timer_period & 0x0700) | (data as u16);
            }
            0x4007 => {
                self.pulse2.timer_period =
                    (self.pulse2.timer_period & 0x00FF) | (((data & 7) as u16) << 8);
                if self.pulse2.enabled {
                    self.pulse2.length_counter = LENGTH_TABLE[(data >> 3) as usize];
                }
                self.pulse2.duty_pos = 0;
                self.pulse2.env.start = true;
            }
            0x4008 => {
                self.triangle.control_flag = (data & 0x80) != 0;
                self.triangle.linear_reload_value = data & 0x7F;
            }
            0x400A => {
                self.triangle.timer_period = (self.triangle.timer_period & 0x0700) | (data as u16);
            }
            0x400B => {
                self.triangle.timer_period =
                    (self.triangle.timer_period & 0x00FF) | (((data & 7) as u16) << 8);
                if self.triangle.enabled {
                    self.triangle.length_counter = LENGTH_TABLE[(data >> 3) as usize];
                }
                self.triangle.linear_reload = true;
            }
            0x400C => {
                self.noise.length_halt = (data & 0x20) != 0;
                self.noise.env.loop_flag = self.noise.length_halt;
                self.noise.env.constant_volume = (data & 0x10) != 0;
                self.noise.env.volume = data & 0x0F;
            }
            0x400E => {
                self.noise.mode = (data & 0x80) != 0;
                self.noise.timer_period = NOISE_PERIOD_TABLE[(data & 0x0F) as usize];
            }
            0x400F => {
                if self.noise.enabled {
                    self.noise.length_counter = LENGTH_TABLE[(data >> 3) as usize];
                }
                self.noise.env.start = true;
            }
            0x4010 => {
                self.dmc.irq_enabled = (data & 0x80) != 0;
                if !self.dmc.irq_enabled {
                    self.dmc.irq_pending = false;
                }
                self.dmc.loop_flag = (data & 0x40) != 0;
                self.dmc.timer_period = DMC_PERIOD_TABLE[(data & 0x0F) as usize];
            }
            0x4011 => {
                self.dmc.output_level = data & 0x7F;
            }
            0x4012 => {
                self.dmc.sample_address = 0xC000 | ((data as u16) << 6);
            }
            0x4013 => {
                self.dmc.sample_length = ((data as u16) << 4) | 1;
            }
            0x4015 => {
                self.pulse1.enabled = (data & 0x01) != 0;
                if !self.pulse1.enabled { self.pulse1.length_counter = 0; }

                self.pulse2.enabled = (data & 0x02) != 0;
                if !self.pulse2.enabled { self.pulse2.length_counter = 0; }

                self.triangle.enabled = (data & 0x04) != 0;
                if !self.triangle.enabled { self.triangle.length_counter = 0; }

                self.noise.enabled = (data & 0x08) != 0;
                if !self.noise.enabled { self.noise.length_counter = 0; }

                self.dmc.enabled = (data & 0x10) != 0;
                if !self.dmc.enabled {
                    self.dmc.bytes_remaining = 0;
                } else if self.dmc.bytes_remaining == 0 {
                    self.dmc.restart();
                }
                self.dmc.irq_pending = false;
            }
            0x4017 => {
                self.frame_counter_mode = (data >> 7) & 1;
                self.frame_step = 0;
                if self.frame_counter_mode == 1 {
                    self.clock_quarter_frame();
                    self.clock_half_frame();
                }
            }
            _ => {}
        }
    }

    pub fn cpu_read(&mut self, addr: u16) -> u8 {
        if addr == 0x4015 {
            let mut status = 0;
            if self.pulse1.length_counter > 0   { status |= 0x01; }
            if self.pulse2.length_counter > 0   { status |= 0x02; }
            if self.triangle.length_counter > 0 { status |= 0x04; }
            if self.noise.length_counter > 0    { status |= 0x08; }
            if self.dmc.bytes_remaining > 0     { status |= 0x10; }
            if self.dmc.irq_pending             { status |= 0x80; }
            self.dmc.irq_pending = false;
            status
        } else {
            0
        }
    }

    fn clock_quarter_frame(&mut self) {
        self.pulse1.env.clock();
        self.pulse2.env.clock();
        self.noise.env.clock();
        self.triangle.clock_linear();
    }

    fn clock_half_frame(&mut self) {
        self.pulse1.clock_length();
        self.pulse1.clock_sweep();
        self.pulse2.clock_length();
        self.pulse2.clock_sweep();
        self.triangle.clock_length();
        self.noise.clock_length();
    }

    pub fn step_cpu_cycle(&mut self) {
        self.cpu_cycle_count += 1;

        self.dmc.clock_timer();
        self.triangle.clock_timer();
        if self.cpu_cycle_count % 2 == 0 {
            self.pulse1.clock_timer();
            self.pulse2.clock_timer();
            self.noise.clock_timer();
        }

        self.frame_step += 1;
        match (self.frame_counter_mode, self.frame_step) {
            (_, 3729) | (_, 11186) => self.clock_quarter_frame(),
            (_, 7457) => {
                self.clock_quarter_frame();
                self.clock_half_frame();
            }
            (0, 14915) => {
                self.clock_quarter_frame();
                self.clock_half_frame();
                self.frame_step = 0;
            }
            (1, 18641) => {
                self.clock_quarter_frame();
                self.clock_half_frame();
                self.frame_step = 0;
            }
            _ => {}
        }

        const CPU_FREQ: f64 = 1_789_773.0;
        self.sample_acc += self.sample_rate;
        if self.sample_acc >= CPU_FREQ {
            self.sample_acc -= CPU_FREQ;
            let raw = self.mix_output();
            let filtered = raw - self.prev_raw_sample + 0.996 * self.high_pass_cap;
            self.prev_raw_sample = raw;
            self.high_pass_cap = filtered;

            if let Ok(mut buf) = self.audio_buffer.try_lock() {
                // Mantém o buffer elástico entre 1024 e 2048 samples para latência zero e sem estalos
                if buf.len() > 2048 {
                    buf.drain(0..512); // Descarta suavemente o atraso acumulado
                }
                buf.push_back(filtered * self.master_volume);
            }
        }
    }

    fn mix_output(&self) -> f32 {
        let p1 = self.pulse1.output() as f32;
        let p2 = self.pulse2.output() as f32;
        let t = self.triangle.output() as f32;
        let n = self.noise.output() as f32;
        let d = self.dmc.output() as f32;

        let pulse_out = if p1 + p2 > 0.0 {
            95.88 / ((8128.0 / (p1 + p2)) + 100.0)
        } else {
            0.0
        };

        let tnd_sum = (t / 8227.0) + (n / 12241.0) + (d / 22638.0);
        let tnd_out = if tnd_sum > 0.0 {
            159.79 / ((1.0 / tnd_sum) + 100.0)
        } else {
            0.0
        };

        pulse_out + tnd_out
    }
}

// ============================================================================
// CARTUCHO E MAPPERS (0, 1, 2, 3, 4, 7)
// ============================================================================
#[derive(Clone, Copy, PartialEq)]
pub enum Mirroring {
    Horizontal,
    Vertical,
    OneScreenLo,
    OneScreenHi,
    FourScreen,
}

#[derive(Clone)]
pub struct Cartridge {
    pub prg_rom: Vec<u8>,
    pub chr_mem: Vec<u8>,
    pub prg_ram: Vec<u8>,
    pub is_chr_ram: bool,
    pub has_battery: bool,
    pub prg_ram_dirty: bool,
    pub mapper: u8,
    pub mirroring: Mirroring,

    pub prg_bank: usize,
    pub chr_bank_0: usize,
    pub chr_bank_1: usize,
    pub mmc1_shift: u8,
    pub mmc1_ctrl: u8,
    pub mmc1_prg_bank: u8,

    pub mmc3_bank_select: u8,
    pub mmc3_registers: [usize; 8],
    pub mmc3_prg_mode: bool,
    pub mmc3_chr_mode: bool,
    pub mmc3_irq_latch: u8,
    pub mmc3_irq_counter: u8,
    pub mmc3_irq_enabled: bool,
    pub mmc3_irq_reload: bool,
    pub mmc3_irq_pending: bool,

    // --- MAPPER 9 / 10 (MMC2 / MMC4 - Punch-Out!! / Fire Emblem) ---
    pub mmc2_prg_bank: usize,
    pub mmc2_latch_0: std::cell::Cell<u8>, // $FD ou $FE
    pub mmc2_latch_1: std::cell::Cell<u8>, // $FD ou $FE
    pub mmc2_chr_banks: [usize; 4], // [FD0, FE0, FD1, FE1]

    // --- MAPPER 11 / 66 (Color Dreams / GxROM) ---
    // (Usa os bancos de PRG e CHR já existentes)

    // --- MAPPER 21 / 22 / 23 / 25 (Konami VRC2 / VRC4 - Contra JP / Ganbare Goemon) ---
    pub vrc_prg_banks: [usize; 2],
    pub vrc_chr_banks: [usize; 8],
    pub vrc_irq_latch: u8,
    pub vrc_irq_counter: u8,
    pub vrc_irq_enabled: bool,
    pub vrc_irq_cycle_mode: bool,
    pub vrc_irq_prescaler: i16,

    // Mapper 69: Sunsoft FME-7
    pub fme7_command: u8,
    pub fme7_prg_banks: [usize; 4],
    pub fme7_prg_ram_select: bool,
    pub fme7_prg_ram_enable: bool,
    pub fme7_chr_banks: [usize; 8],
    pub fme7_irq_enabled: bool,
    pub fme7_irq_counter_enable: bool,
    pub fme7_irq_counter: u16,

    // Mapper 5: Nintendo MMC5
    pub mmc5_prg_mode: u8,
    pub mmc5_chr_mode: u8,
    pub mmc5_prg_banks: [usize; 5],
    pub mmc5_chr_banks: [usize; 12],
    pub mmc5_chr_upper: usize,
    pub mmc5_last_chr_set: u8,
    pub mmc5_exram: Vec<u8>,
    pub mmc5_exram_mode: u8,
    pub mmc5_mult_a: u8,
    pub mmc5_mult_b: u8,
    pub mmc5_irq_target: u8,
    pub mmc5_irq_enabled: bool,
    pub mmc5_irq_pending: std::cell::Cell<bool>,
    pub mmc5_in_frame: std::cell::Cell<bool>,
    pub mmc5_scanline: u8,
    pub mmc5_fill_tile: u8,
    pub mmc5_fill_attr: u8,
}

impl Cartridge {
    pub fn from_ines(raw: &[u8]) -> Result<Self, String> {
        if raw.len() < 16 || &raw[0..4] != b"NES\x1A" {
            return Err("Arquivo invalido: Assinatura iNES nao encontrada.".to_string());
        }

        let prg_banks = raw[4] as usize;
        let chr_banks = raw[5] as usize;
        let flags6 = raw[6];
        let flags7 = raw[7];

        let mapper = (flags7 & 0xF0) | (flags6 >> 4);
        let has_battery = (flags6 & 0b0010) != 0;

        let mirroring = if (flags6 & 0b1000) != 0 {
            Mirroring::FourScreen
        } else if (flags6 & 0b0001) != 0 {
            Mirroring::Vertical
        } else {
            Mirroring::Horizontal
        };

        let has_trainer = (flags6 & 0b0100) != 0;
        let prg_start = 16 + if has_trainer { 512 } else { 0 };
        let prg_size = prg_banks * 16384;
        let chr_start = prg_start + prg_size;
        let chr_size = chr_banks * 8192;

        if raw.len() < prg_start + prg_size {
            return Err("Arquivo .nes truncado ou corrompido.".to_string());
        }

        let prg_rom = raw[prg_start..(prg_start + prg_size)].to_vec();
        let (chr_mem, is_chr_ram) = if chr_banks == 0 || raw.len() < chr_start + chr_size {
            (vec![0; 8192], true)
        } else {
            (raw[chr_start..(chr_start + chr_size)].to_vec(), false)
        };

        Ok(Self {
            prg_rom,
            chr_mem,
            prg_ram: vec![0; if mapper == 5 { 65536 } else { 8192 }],
            is_chr_ram,
            has_battery,
            prg_ram_dirty: false,
            mapper,
            mirroring,
            prg_bank: 0,
            chr_bank_0: 0,
            chr_bank_1: 0,
            mmc1_shift: 0x10,
            mmc1_ctrl: 0x0C,
            mmc1_prg_bank: 0,
            mmc3_bank_select: 0,
            mmc3_registers: [0, 2, 4, 5, 6, 7, 0, 1],
            mmc3_prg_mode: false,
            mmc3_chr_mode: false,
            mmc3_irq_latch: 0,
            mmc3_irq_counter: 0,
            mmc3_irq_enabled: false,
            mmc3_irq_reload: false,
            mmc3_irq_pending: false,
            mmc2_prg_bank: 0,
            mmc2_latch_0: std::cell::Cell::new(0xFE),
            mmc2_latch_1: std::cell::Cell::new(0xFE),
            mmc2_chr_banks: [0; 4],

            vrc_prg_banks: [0, 1],
            vrc_chr_banks: [0; 8],
            vrc_irq_latch: 0,
            vrc_irq_counter: 0,
            vrc_irq_enabled: false,
            vrc_irq_cycle_mode: false,
            vrc_irq_prescaler: 341,
            fme7_command: 0,
            fme7_prg_banks: [0, 0, 1, 2],
            fme7_prg_ram_select: false,
            fme7_prg_ram_enable: false,
            fme7_chr_banks: [0; 8],
            fme7_irq_enabled: false,
            fme7_irq_counter_enable: false,
            fme7_irq_counter: 0,
            mmc5_prg_mode: 3,
            mmc5_chr_mode: 3,
            mmc5_prg_banks: [0xFF; 5],
            mmc5_chr_banks: [0; 12],
            mmc5_chr_upper: 0,
            mmc5_last_chr_set: 0,
            mmc5_exram: vec![0; 1024],
            mmc5_exram_mode: 0,
            mmc5_mult_a: 0xFF,
            mmc5_mult_b: 0xFF,
            mmc5_irq_target: 0,
            mmc5_irq_enabled: false,
            mmc5_irq_pending: std::cell::Cell::new(false),
            mmc5_in_frame: std::cell::Cell::new(false),
            mmc5_scanline: 0,
            mmc5_fill_tile: 0,
            mmc5_fill_attr: 0,
        })
    }

    pub fn cpu_read(&self, addr: u16) -> u8 {
        match addr {
            0x5204 if self.mapper == 5 => {
                let status = (if self.mmc5_irq_pending.get() { 0x80 } else { 0 })
                    | (if self.mmc5_in_frame.get() { 0x40 } else { 0 });
                self.mmc5_irq_pending.set(false);
                status
            }
            0x5205 if self.mapper == 5 => ((self.mmc5_mult_a as u16 * self.mmc5_mult_b as u16) & 0xFF) as u8,
            0x5206 if self.mapper == 5 => ((self.mmc5_mult_a as u16 * self.mmc5_mult_b as u16) >> 8) as u8,
            0x5C00..=0x5FFF if self.mapper == 5 => self.mmc5_exram[(addr - 0x5C00) as usize],
            0x5000..=0x5FFF => 0,
            0x6000..=0x7FFF if self.mapper == 69 => {
                if self.fme7_prg_ram_select {
                    if self.fme7_prg_ram_enable { self.prg_ram[(addr - 0x6000) as usize % self.prg_ram.len()] } else { 0 }
                } else {
                    let bank = self.fme7_prg_banks[0] % (self.prg_rom.len() / 0x2000).max(1);
                    self.prg_rom[(bank * 0x2000 + (addr as usize & 0x1FFF)) % self.prg_rom.len()]
                }
            }
            0x6000..=0x7FFF if self.mapper == 5 => {
                let bank = self.mmc5_prg_banks[0] & 0x07;
                self.prg_ram[(bank * 0x2000 + (addr as usize & 0x1FFF)) % self.prg_ram.len()]
            }
            0x6000..=0x7FFF => self.prg_ram[(addr - 0x6000) as usize],
            0x8000..=0xFFFF => {
                let total_32k = (self.prg_rom.len() / 0x8000).max(1);
                let total_16k = (self.prg_rom.len() / 0x4000).max(1);
                let total_8k = (self.prg_rom.len() / 0x2000).max(1);
                match self.mapper {
                    0 | 3 => {
                        let mut offset = (addr - 0x8000) as usize;
                        if self.prg_rom.len() == 0x4000 {
                            offset &= 0x3FFF;
                        }
                        self.prg_rom[offset % self.prg_rom.len()]
                    }
                    1 => {
                        let prg_mode = (self.mmc1_ctrl >> 2) & 0x03;
                        let offset = match prg_mode {
                            0 | 1 => {
                                let bank32 = (self.mmc1_prg_bank as usize & 0x0E) >> 1;
                                bank32 * 0x8000 + (addr as usize - 0x8000)
                            }
                            2 => {
                                if addr < 0xC000 {
                                    addr as usize - 0x8000
                                } else {
                                    let bank = (self.mmc1_prg_bank as usize) % total_16k;
                                    bank * 0x4000 + (addr as usize - 0xC000)
                                }
                            }
                            _ => {
                                if addr < 0xC000 {
                                    let bank = (self.mmc1_prg_bank as usize) % total_16k;
                                    bank * 0x4000 + (addr as usize - 0x8000)
                                } else {
                                    (total_16k - 1) * 0x4000 + (addr as usize - 0xC000)
                                }
                            }
                        };
                        self.prg_rom[offset % self.prg_rom.len()]
                    }
                    2 | 71 => {
                        let offset = if addr < 0xC000 {
                            let bank = self.prg_bank % total_16k;
                            bank * 0x4000 + (addr as usize - 0x8000)
                        } else {
                            (total_16k - 1) * 0x4000 + (addr as usize - 0xC000)
                        };
                        self.prg_rom[offset % self.prg_rom.len()]
                    }
                    4 => {
                        let bank = match (addr, self.mmc3_prg_mode) {
                            (0x8000..=0x9FFF, false) => self.mmc3_registers[6],
                            (0x8000..=0x9FFF, true)  => total_8k.saturating_sub(2),
                            (0xA000..=0xBFFF, _)     => self.mmc3_registers[7],
                            (0xC000..=0xDFFF, false) => total_8k.saturating_sub(2),
                            (0xC000..=0xDFFF, true)  => self.mmc3_registers[6],
                            (0xE000..=0xFFFF, _)     => total_8k.saturating_sub(1),
                            _ => 0,
                        } % total_8k;
                        let offset = bank * 0x2000 + ((addr & 0x1FFF) as usize);
                        self.prg_rom[offset % self.prg_rom.len()]
                    }
                    5 => {
                        let total_32k = (self.prg_rom.len() / 0x8000).max(1);
                        let total_16k = (self.prg_rom.len() / 0x4000).max(1);
                        let total_8k = (self.prg_rom.len() / 0x2000).max(1);
                        let bank_offset = match self.mmc5_prg_mode & 3 {
                            0 => ((self.mmc5_prg_banks[4] & 0x7C) >> 2) % total_32k * 0x8000 + (addr as usize - 0x8000),
                            1 if addr < 0xC000 => {
                                let raw = self.mmc5_prg_banks[2];
                                if raw & 0x80 == 0 { let bank = (raw & 6) >> 1; return self.prg_ram[(bank * 0x4000 + (addr as usize - 0x8000)) % self.prg_ram.len()]; }
                                ((raw & 0x7E) >> 1) % total_16k * 0x4000 + (addr as usize - 0x8000)
                            }
                            1 => ((self.mmc5_prg_banks[4] & 0x7E) >> 1) % total_16k * 0x4000 + (addr as usize - 0xC000),
                            2 if addr < 0xC000 => {
                                let raw = self.mmc5_prg_banks[2];
                                if raw & 0x80 == 0 { let bank = (raw & 6) >> 1; return self.prg_ram[(bank * 0x4000 + (addr as usize - 0x8000)) % self.prg_ram.len()]; }
                                ((raw & 0x7E) >> 1) % total_16k * 0x4000 + (addr as usize - 0x8000)
                            }
                            2 if addr < 0xE000 => {
                                let raw = self.mmc5_prg_banks[3];
                                if raw & 0x80 == 0 { return self.prg_ram[((raw & 7) * 0x2000 + (addr as usize & 0x1FFF)) % self.prg_ram.len()]; }
                                (raw & 0x7F) % total_8k * 0x2000 + (addr as usize & 0x1FFF)
                            }
                            2 => (self.mmc5_prg_banks[4] & 0x7F) % total_8k * 0x2000 + (addr as usize & 0x1FFF),
                            _ => {
                                let idx = ((addr - 0x8000) / 0x2000) as usize + 1;
                                let raw = self.mmc5_prg_banks[idx];
                                if idx < 4 && raw & 0x80 == 0 { return self.prg_ram[((raw & 7) * 0x2000 + (addr as usize & 0x1FFF)) % self.prg_ram.len()]; }
                                (raw & 0x7F) % total_8k * 0x2000 + (addr as usize & 0x1FFF)
                            }
                        };
                        self.prg_rom[bank_offset % self.prg_rom.len()]
                    }
                    69 => {
                        let total_8k = (self.prg_rom.len() / 0x2000).max(1);
                        let bank = match addr {
                            0x8000..=0x9FFF => self.fme7_prg_banks[1],
                            0xA000..=0xBFFF => self.fme7_prg_banks[2],
                            0xC000..=0xDFFF => self.fme7_prg_banks[3],
                            _ => total_8k.saturating_sub(1),
                        } % total_8k;
                        self.prg_rom[(bank * 0x2000 + (addr as usize & 0x1FFF)) % self.prg_rom.len()]
                    }
                    7 => {
                        let bank32 = self.prg_bank % total_32k;
                        let offset = bank32 * 0x8000 + (addr as usize - 0x8000);
                        self.prg_rom[offset % self.prg_rom.len()]
                    }
                    // MAPPER 9 (MMC2 - Mike Tyson's Punch-Out!!)
                    9 => {
                        let offset = match addr {
                            0x8000..=0x9FFF => (self.mmc2_prg_bank * 0x2000) + (addr as usize - 0x8000),
                            0xA000..=0xBFFF => (total_8k.saturating_sub(3) * 0x2000) + (addr as usize - 0xA000),
                            0xC000..=0xDFFF => (total_8k.saturating_sub(2) * 0x2000) + (addr as usize - 0xC000),
                            0xE000..=0xFFFF => (total_8k.saturating_sub(1) * 0x2000) + (addr as usize - 0xE000),
                            _ => 0,
                        };
                        self.prg_rom[offset % self.prg_rom.len()]
                    }

                    // MAPPER 10 (MMC4 - Fire Emblem)
                    10 => {
                        let offset = match addr {
                            0x8000..=0xBFFF => (self.mmc2_prg_bank * 0x4000) + (addr as usize - 0x8000),
                            0xC000..=0xFFFF => (total_16k.saturating_sub(1) * 0x4000) + (addr as usize - 0xC000),
                            _ => 0,
                        };
                        self.prg_rom[offset % self.prg_rom.len()]
                    }

                    // MAPPER 11 (Color Dreams) e 66 (GxROM - Doraemon, Super Mario Bros/Duck Hunt)
                    11 | 66 => {
                        let bank32 = self.prg_bank % total_32k;
                        let offset = bank32 * 0x8000 + (addr as usize - 0x8000);
                        self.prg_rom[offset % self.prg_rom.len()]
                    }

                    // MAPPER 21, 22, 23, 25 (Konami VRC2 & VRC4)
                    21 | 22 | 23 | 25 => {
                        let offset = match addr {
                            0x8000..=0x9FFF => (self.vrc_prg_banks[0] % total_8k) * 0x2000 + (addr as usize - 0x8000),
                            0xA000..=0xBFFF => (self.vrc_prg_banks[1] % total_8k) * 0x2000 + (addr as usize - 0xA000),
                            0xC000..=0xDFFF => (total_8k.saturating_sub(2)) * 0x2000 + (addr as usize - 0xC000),
                            0xE000..=0xFFFF => (total_8k.saturating_sub(1)) * 0x2000 + (addr as usize - 0xE000),
                            _ => 0,
                        };
                        self.prg_rom[offset % self.prg_rom.len()]
                    }
                    _ => self.prg_rom[(addr as usize - 0x8000) % self.prg_rom.len()],
                }
            }
            _ => 0,
        }
    }

    pub fn cpu_write(&mut self, addr: u16, data: u8) {
        match addr {
            0x5100..=0x5FFF if self.mapper == 5 => match addr {
                0x5100 => self.mmc5_prg_mode = data & 3,
                0x5101 => self.mmc5_chr_mode = data & 3,
                0x5104 => self.mmc5_exram_mode = data & 3,
                0x5105 => self.mirroring = match data & 3 { 0 => Mirroring::OneScreenLo, 1 => Mirroring::OneScreenHi, 2 => Mirroring::Vertical, _ => Mirroring::Horizontal },
                0x5106 => self.mmc5_fill_tile = data,
                0x5107 => self.mmc5_fill_attr = data & 3,
                0x5113..=0x5117 => self.mmc5_prg_banks[(addr - 0x5113) as usize] = data as usize,
                0x5120..=0x5127 => { self.mmc5_chr_banks[(addr - 0x5120) as usize] = (self.mmc5_chr_upper << 8) | data as usize; self.mmc5_last_chr_set = 0; }
                0x5128..=0x512B => { self.mmc5_chr_banks[8 + (addr - 0x5128) as usize] = (self.mmc5_chr_upper << 8) | data as usize; self.mmc5_last_chr_set = 1; }
                0x5130 => self.mmc5_chr_upper = (data & 3) as usize,
                0x5203 => self.mmc5_irq_target = data,
                0x5204 => { self.mmc5_irq_enabled = data & 0x80 != 0; self.mmc5_irq_pending.set(false); self.mmc3_irq_pending = false; }
                0x5205 => self.mmc5_mult_a = data,
                0x5206 => self.mmc5_mult_b = data,
                0x5C00..=0x5FFF => self.mmc5_exram[(addr - 0x5C00) as usize] = data,
                _ => {}
            },
            0x8000..=0x9FFF if self.mapper == 69 => self.fme7_command = data & 0x0F,
            0xA000..=0xBFFF if self.mapper == 69 => match self.fme7_command {
                0..=7 => self.fme7_chr_banks[self.fme7_command as usize] = data as usize,
                8 => { self.fme7_prg_banks[0] = (data & 0x3F) as usize; self.fme7_prg_ram_select = data & 0x40 != 0; self.fme7_prg_ram_enable = data & 0x80 != 0; }
                9..=11 => self.fme7_prg_banks[(self.fme7_command - 8) as usize] = (data & 0x3F) as usize,
                12 => self.mirroring = match data & 3 { 0 => Mirroring::Vertical, 1 => Mirroring::Horizontal, 2 => Mirroring::OneScreenLo, _ => Mirroring::OneScreenHi },
                13 => { self.fme7_irq_enabled = data & 1 != 0; self.fme7_irq_counter_enable = data & 0x80 != 0; self.mmc3_irq_pending = false; }
                14 => self.fme7_irq_counter = (self.fme7_irq_counter & 0xFF00) | data as u16,
                15 => self.fme7_irq_counter = (self.fme7_irq_counter & 0x00FF) | ((data as u16) << 8),
                _ => {}
            },
            0x8000..=0x9FFF if self.mapper == 71 => {
                if addr >= 0x9000 { self.mirroring = if data & 0x10 != 0 { Mirroring::OneScreenHi } else { Mirroring::OneScreenLo }; }
            }
            0xC000..=0xFFFF if self.mapper == 71 => self.prg_bank = (data & 0x0F) as usize,
            0x6000..=0x7FFF => {
                if self.mapper == 5 {
                    let bank = self.mmc5_prg_banks[0] & 7;
                    let idx = (bank * 0x2000 + (addr as usize & 0x1FFF)) % self.prg_ram.len();
                    self.prg_ram[idx] = data;
                    self.prg_ram_dirty = true;
                } else if self.mapper != 69 || (self.fme7_prg_ram_select && self.fme7_prg_ram_enable) {
                    let index = (addr - 0x6000) as usize % self.prg_ram.len();
                    self.prg_ram[index] = data;
                    self.prg_ram_dirty = true;
                }
            }
            0x8000..=0xFFFF => match self.mapper {
                1 => {
                    if (data & 0x80) != 0 {
                        self.mmc1_shift = 0x10;
                        self.mmc1_ctrl |= 0x0C;
                    } else {
                        let complete = (self.mmc1_shift & 1) != 0;
                        self.mmc1_shift = (self.mmc1_shift >> 1) | ((data & 1) << 4);
                        if complete {
                            let val = self.mmc1_shift;
                            self.mmc1_shift = 0x10;
                            match addr {
                                0x8000..=0x9FFF => {
                                    self.mmc1_ctrl = val;
                                    self.mirroring = match val & 0x03 {
                                        0 => Mirroring::OneScreenLo,
                                        1 => Mirroring::OneScreenHi,
                                        2 => Mirroring::Vertical,
                                        _ => Mirroring::Horizontal,
                                    };
                                }
                                0xA000..=0xBFFF => self.chr_bank_0 = val as usize,
                                0xC000..=0xDFFF => self.chr_bank_1 = val as usize,
                                0xE000..=0xFFFF => self.mmc1_prg_bank = val & 0x0F,
                                _ => {}
                            }
                        }
                    }
                }
                2 => {
                    self.prg_bank = (data & 0x0F) as usize;
                }
                3 => {
                    self.chr_bank_0 = (data & 0x03) as usize;
                }
                4 => {
                    match addr {
                        0x8000..=0x9FFF => {
                            if addr % 2 == 0 {
                                self.mmc3_bank_select = data & 0x07;
                                self.mmc3_prg_mode = (data & 0x40) != 0;
                                self.mmc3_chr_mode = (data & 0x80) != 0;
                            } else {
                                let reg = self.mmc3_bank_select as usize;
                                self.mmc3_registers[reg] = data as usize;
                            }
                        }
                        0xA000..=0xBFFF => {
                            if addr % 2 == 0 {
                                self.mirroring = if (data & 1) == 0 {
                                    Mirroring::Vertical
                                } else {
                                    Mirroring::Horizontal
                                };
                            }
                        }
                        0xC000..=0xDFFF => {
                            if addr % 2 == 0 {
                                self.mmc3_irq_latch = data;
                            } else {
                                self.mmc3_irq_reload = true;
                            }
                        }
                        0xE000..=0xFFFF => {
                            if addr % 2 == 0 {
                                self.mmc3_irq_enabled = false;
                                self.mmc3_irq_pending = false;
                            } else {
                                self.mmc3_irq_enabled = true;
                            }
                        }
                        _ => {}
                    }
                }
                7 => {
                    self.prg_bank = (data & 0x07) as usize;
                    self.mirroring = if (data & 0x10) != 0 {
                        Mirroring::OneScreenHi
                    } else {
                        Mirroring::OneScreenLo
                    };
                }

                // MAPPER 9 & 10 (MMC2 / MMC4)
                9 | 10 => {
                    match addr {
                        0xA000..=0xAFFF => self.mmc2_prg_bank = (data & 0x0F) as usize,
                        0xB000..=0xBFFF => self.mmc2_chr_banks[0] = (data & 0x1F) as usize, // $FD banco 0
                        0xC000..=0xCFFF => self.mmc2_chr_banks[1] = (data & 0x1F) as usize, // $FE banco 0
                        0xD000..=0xDFFF => self.mmc2_chr_banks[2] = (data & 0x1F) as usize, // $FD banco 1
                        0xE000..=0xEFFF => self.mmc2_chr_banks[3] = (data & 0x1F) as usize, // $FE banco 1
                        0xF000..=0xFFFF => {
                            self.mirroring = if (data & 1) == 0 {
                                Mirroring::Vertical
                            } else {
                                Mirroring::Horizontal
                            };
                        }
                        _ => {}
                    }
                }

                // MAPPER 11 (Color Dreams): Bits 0-1 PRG, Bits 4-7 CHR
                11 => {
                    self.prg_bank = (data & 0x03) as usize;
                    self.chr_bank_0 = ((data >> 4) & 0x0F) as usize;
                }

                // MAPPER 66 (GxROM): Bits 4-5 PRG (32K), Bits 0-1 CHR (8K)
                66 => {
                    self.prg_bank = ((data >> 4) & 0x03) as usize;
                    self.chr_bank_0 = (data & 0x03) as usize;
                }

                // MAPPER 21, 22, 23, 25 (Konami VRC2 & VRC4)
                21 | 22 | 23 | 25 => {
                    // Decodificação universal das linhas de endereço da Konami
                    let reg = addr & 0xF000;
                    let sub = (addr & 0x000F) as u8;
                    match reg {
                        0x8000 => self.vrc_prg_banks[0] = (data & 0x1F) as usize,
                        0x9000 => {
                            if sub == 0 || sub == 1 || sub == 2 || sub == 3 {
                                self.mirroring = match data & 0x03 {
                                    0 => Mirroring::Vertical,
                                    1 => Mirroring::Horizontal,
                                    2 => Mirroring::OneScreenLo,
                                    _ => Mirroring::OneScreenHi,
                                };
                            }
                        }
                        0xA000 => self.vrc_prg_banks[1] = (data & 0x1F) as usize,
                        0xB000..=0xE000 => {
                            // Bancos CHR de 1KB
                            let bank_idx = (((reg - 0xB000) / 0x1000) * 2) as usize + if (sub & 0x02) != 0 { 1 } else { 0 };
                            if bank_idx < 8 {
                                if (sub & 0x01) == 0 {
                                    self.vrc_chr_banks[bank_idx] = (self.vrc_chr_banks[bank_idx] & 0x1F0) | ((data & 0x0F) as usize);
                                } else {
                                    self.vrc_chr_banks[bank_idx] = (self.vrc_chr_banks[bank_idx] & 0x00F) | (((data & 0x1F) as usize) << 4);
                                }
                            }
                        }
                        0xF000 => {
                            // Controle de IRQ VRC
                            match sub & 0x03 {
                                0 => self.vrc_irq_latch = (self.vrc_irq_latch & 0xF0) | (data & 0x0F),
                                1 => self.vrc_irq_latch = (self.vrc_irq_latch & 0x0F) | ((data & 0x0F) << 4),
                                2 => {
                                    self.vrc_irq_enabled = (data & 0x02) != 0;
                                    self.vrc_irq_cycle_mode = (data & 0x04) != 0;
                                    if (data & 0x02) != 0 {
                                        self.vrc_irq_counter = self.vrc_irq_latch;
                                        self.vrc_irq_prescaler = 341;
                                    }
                                    self.mmc3_irq_pending = false; // Compartilha linha IRQ
                                }
                                3 => {
                                    self.vrc_irq_enabled = (data & 0x01) != 0;
                                    self.mmc3_irq_pending = false;
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    pub fn ppu_read(&self, addr: u16) -> u8 {
        let addr = addr as usize;
        match self.mapper {
            5 if !self.is_chr_ram => {
                let slot = addr / 0x0400;
                let offset = match self.mmc5_chr_mode & 3 {
                    0 => self.mmc5_chr_banks[7] * 0x2000 + addr,
                    1 => { let reg = if addr < 0x1000 { 3 } else { 7 }; self.mmc5_chr_banks[reg] * 0x1000 + (addr & 0x0FFF) }
                    2 => { let reg = (slot & !1) + 1; self.mmc5_chr_banks[reg] * 0x0800 + (addr & 0x07FF) }
                    _ => {
                        let reg = if self.mmc5_last_chr_set == 1 && slot >= 4 { 8 + (slot & 3) } else { slot };
                        self.mmc5_chr_banks[reg] * 0x0400 + (addr & 0x03FF)
                    }
                };
                self.chr_mem[offset % self.chr_mem.len()]
            }
            69 if !self.is_chr_ram => {
                let slot = addr / 0x0400;
                let offset = self.fme7_chr_banks[slot] * 0x0400 + (addr & 0x03FF);
                self.chr_mem[offset % self.chr_mem.len()]
            }
            1 if !self.is_chr_ram => {
                let chr_mode = (self.mmc1_ctrl >> 4) & 1;
                let offset = if chr_mode == 0 {
                    let bank8 = self.chr_bank_0 & 0x1E;
                    bank8 * 0x1000 + addr
                } else if addr < 0x1000 {
                    self.chr_bank_0 * 0x1000 + addr
                } else {
                    self.chr_bank_1 * 0x1000 + (addr - 0x1000)
                };
                self.chr_mem[offset % self.chr_mem.len()]
            }
            3 if !self.is_chr_ram => {
                let offset = self.chr_bank_0 * 0x2000 + addr;
                self.chr_mem[offset % self.chr_mem.len()]
            }
            4 => {
                let offset = if !self.mmc3_chr_mode {
                    match addr {
                        0x0000..=0x07FF => (self.mmc3_registers[0] & 0xFE) * 0x0400 + addr,
                        0x0800..=0x0FFF => (self.mmc3_registers[1] & 0xFE) * 0x0400 + (addr - 0x0800),
                        0x1000..=0x13FF => self.mmc3_registers[2] * 0x0400 + (addr - 0x1000),
                        0x1400..=0x17FF => self.mmc3_registers[3] * 0x0400 + (addr - 0x1400),
                        0x1800..=0x1BFF => self.mmc3_registers[4] * 0x0400 + (addr - 0x1800),
                        0x1C00..=0x1FFF => self.mmc3_registers[5] * 0x0400 + (addr - 0x1C00),
                        _ => addr,
                    }
                } else {
                    match addr {
                        0x0000..=0x03FF => self.mmc3_registers[2] * 0x0400 + addr,
                        0x0400..=0x07FF => self.mmc3_registers[3] * 0x0400 + (addr - 0x0400),
                        0x0800..=0x0BFF => self.mmc3_registers[4] * 0x0400 + (addr - 0x0800),
                        0x0C00..=0x0FFF => self.mmc3_registers[5] * 0x0400 + (addr - 0x0C00),
                        0x1000..=0x17FF => (self.mmc3_registers[0] & 0xFE) * 0x0400 + (addr - 0x1000),
                        0x1800..=0x1FFF => (self.mmc3_registers[1] & 0xFE) * 0x0400 + (addr - 0x1800),
                        _ => addr,
                    }
                };
                self.chr_mem[offset % self.chr_mem.len()]
            }
            // MAPPER 9 (MMC2 - Punch-Out!!)
            9 if !self.is_chr_ram => {
                let bank = if addr < 0x1000 {
                    if self.mmc2_latch_0.get() == 0xFD { self.mmc2_chr_banks[0] } else { self.mmc2_chr_banks[1] }
                } else {
                    if self.mmc2_latch_1.get() == 0xFD { self.mmc2_chr_banks[2] } else { self.mmc2_chr_banks[3] }
                };
                let offset = bank * 0x1000 + (addr as usize & 0x0FFF);
                let val = self.chr_mem[offset % self.chr_mem.len()];

                // Travas de hardware (Latches) do Punch-Out!!
                if addr == 0x0FD8 { self.mmc2_latch_0.set(0xFD); }
                else if addr == 0x0FE8 { self.mmc2_latch_0.set(0xFE); }
                else if (0x1FD8..=0x1FDF).contains(&addr) { self.mmc2_latch_1.set(0xFD); }
                else if (0x1FE8..=0x1FEF).contains(&addr) { self.mmc2_latch_1.set(0xFE); }

                val
            }

            // MAPPER 10 (MMC4 - Fire Emblem)
            10 if !self.is_chr_ram => {
                let bank = if addr < 0x1000 {
                    if self.mmc2_latch_0.get() == 0xFD { self.mmc2_chr_banks[0] } else { self.mmc2_chr_banks[1] }
                } else {
                    if self.mmc2_latch_1.get() == 0xFD { self.mmc2_chr_banks[2] } else { self.mmc2_chr_banks[3] }
                };
                let offset = bank * 0x1000 + (addr as usize & 0x0FFF);
                let val = self.chr_mem[offset % self.chr_mem.len()];

                if (0x0FD8..=0x0FDF).contains(&addr) { self.mmc2_latch_0.set(0xFD); }
                else if (0x0FE8..=0x0FEF).contains(&addr) { self.mmc2_latch_0.set(0xFE); }
                else if (0x1FD8..=0x1FDF).contains(&addr) { self.mmc2_latch_1.set(0xFD); }
                else if (0x1FE8..=0x1FEF).contains(&addr) { self.mmc2_latch_1.set(0xFE); }

                val
            }

            // MAPPER 11 & 66 (Color Dreams & GxROM)
            11 | 66 if !self.is_chr_ram => {
                let offset = self.chr_bank_0 * 0x2000 + addr as usize;
                self.chr_mem[offset % self.chr_mem.len()]
            }

            // MAPPER 21, 22, 23, 25 (Konami VRC2 / VRC4 - 8 bancos de 1KB)
            21 | 22 | 23 | 25 if !self.is_chr_ram => {
                let slot = (addr / 0x0400) as usize;
                let offset = (self.vrc_chr_banks[slot] * 0x0400) + (addr as usize & 0x03FF);
                self.chr_mem[offset % self.chr_mem.len()]
            }
            _ => self.chr_mem[addr % self.chr_mem.len()],
        }
    }

    pub fn clock_scanline_irq(&mut self) {
        if self.mapper == 4 {
            if self.mmc3_irq_counter == 0 || self.mmc3_irq_reload {
                self.mmc3_irq_counter = self.mmc3_irq_latch;
                self.mmc3_irq_reload = false;
            } else {
                self.mmc3_irq_counter = self.mmc3_irq_counter.saturating_sub(1);
            }
            if self.mmc3_irq_counter == 0 && self.mmc3_irq_enabled {
                self.mmc3_irq_pending = true;
            }
        } else if matches!(self.mapper, 21 | 22 | 23 | 25) && self.vrc_irq_enabled && !self.vrc_irq_cycle_mode {
            self.clock_vrc_irq();
        }
        if self.mapper == 5 {
            if !self.mmc5_in_frame.get() {
                self.mmc5_in_frame.set(true);
                self.mmc5_scanline = 0;
                self.mmc5_irq_pending.set(false);
            } else {
                self.mmc5_scanline = self.mmc5_scanline.wrapping_add(1);
                if self.mmc5_scanline == self.mmc5_irq_target { self.mmc5_irq_pending.set(true); }
            }
            if self.mmc5_irq_enabled && self.mmc5_irq_pending.get() { self.mmc3_irq_pending = true; }
        }
    }

    pub fn clock_vrc_irq(&mut self) {
        if self.vrc_irq_counter == 0xFF {
            self.vrc_irq_counter = self.vrc_irq_latch;
            self.mmc3_irq_pending = true;
        } else {
            self.vrc_irq_counter += 1;
        }
    }

    pub fn ppu_write(&mut self, addr: u16, data: u8) {
        if self.is_chr_ram {
            let len = self.chr_mem.len();
            self.chr_mem[(addr as usize) % len] = data;
        }
    }
}

// ============================================================================
// CONTROLE PADRAO DO NES ($4016)
// ============================================================================
#[derive(Clone)]
pub struct Joypad {
    pub strobe: bool,
    pub button_index: u8,
    pub status: u8,
}

impl Joypad {
    pub fn new() -> Self {
        Self {
            strobe: false,
            button_index: 0,
            status: 0,
        }
    }

    pub fn write(&mut self, data: u8) {
        self.strobe = (data & 1) == 1;
        if self.strobe {
            self.button_index = 0;
        }
    }

    pub fn read(&mut self) -> u8 {
        let mut response = 1;
        if self.button_index < 8 {
            response = (self.status >> self.button_index) & 1;
            if !self.strobe {
                self.button_index += 1;
            }
        }
        response | 0x40
    }
}

// ============================================================================
// PPU 2C02 (Picture Processing Unit)
// ============================================================================
#[derive(Clone)]
pub struct Ppu {
    pub vram: [u8; 2048],
    pub palette_table: [u8; 32],
    pub oam_data: [u8; 256],
    pub ctrl: u8,
    pub mask: u8,
    pub status: u8,
    pub oam_addr: u8,
    pub v: u16,
    pub t: u16,
    pub x: u8,
    pub w: bool,
    pub data_buffer: u8,
    pub scanline: i16,
    pub cycle: usize,
    pub nmi_interrupt: bool,
    pub frame_complete: bool,
    pub is_odd_frame: bool,
    pub sprite_limit_enabled: bool,
    pub scanline_sprites: [u8; 64],
    pub scanline_sprite_count: usize,
    pub framebuffer: Box<[u32; 256 * 240]>,
}

impl Ppu {
    pub fn new() -> Self {
        Self {
            vram: [0; 2048],
            palette_table: [0; 32],
            oam_data: [0; 256],
            ctrl: 0,
            mask: 0,
            status: 0,
            oam_addr: 0,
            v: 0,
            t: 0,
            x: 0,
            w: false,
            data_buffer: 0,
            scanline: 0,
            cycle: 0,
            nmi_interrupt: false,
            frame_complete: false,
            is_odd_frame: false,
            sprite_limit_enabled: true,
            scanline_sprites: [0; 64],
            scanline_sprite_count: 0,
            framebuffer: Box::new([0; 256 * 240]),
        }
    }

    fn mirror_vram_addr(&self, addr: u16, mirroring: Mirroring) -> usize {
        let index = ((addr & 0x2FFF) - 0x2000) as usize;
        let table = index / 0x0400;
        let offset = index % 0x0400;
        let mapped_table = match (mirroring, table) {
            (Mirroring::OneScreenLo, _) => 0,
            (Mirroring::OneScreenHi, _) => 1,
            (Mirroring::Horizontal, 0 | 1) => 0,
            (Mirroring::Horizontal, _) => 1,
            (Mirroring::Vertical, 0 | 2) => 0,
            (Mirroring::Vertical, _) => 1,
            (Mirroring::FourScreen, _) => table & 1,
        };
        mapped_table * 0x0400 + offset
    }

    pub fn ppu_read(&self, addr: u16, cart: &Cartridge) -> u8 {
        let addr = addr & 0x3FFF;
        match addr {
            0x0000..=0x1FFF => cart.ppu_read(addr),
            0x2000..=0x3EFF => self.vram[self.mirror_vram_addr(addr, cart.mirroring)],
            0x3F00..=0x3FFF => {
                let mut p_addr = (addr & 0x001F) as usize;
                if p_addr >= 16 && p_addr % 4 == 0 {
                    p_addr -= 16;
                }
                self.palette_table[p_addr]
            }
            _ => 0,
        }
    }

    pub fn ppu_write(&mut self, addr: u16, data: u8, cart: &mut Cartridge) {
        let addr = addr & 0x3FFF;
        match addr {
            0x0000..=0x1FFF => cart.ppu_write(addr, data),
            0x2000..=0x3EFF => {
                let idx = self.mirror_vram_addr(addr, cart.mirroring);
                self.vram[idx] = data;
            }
            0x3F00..=0x3FFF => {
                let mut p_addr = (addr & 0x001F) as usize;
                if p_addr >= 16 && p_addr % 4 == 0 {
                    p_addr -= 16;
                }
                self.palette_table[p_addr] = data;
            }
            _ => {}
        }
    }

    pub fn cpu_read(&mut self, addr: u16, cart: &Cartridge) -> u8 {
        match addr {
            0x2002 => {
                let data = (self.status & 0xE0) | (self.data_buffer & 0x1F);
                self.status &= !0x80;
                self.w = false;
                data
            }
            0x2004 => self.oam_data[self.oam_addr as usize],
            0x2007 => {
                let v = self.v & 0x3FFF;
                let mut data = self.data_buffer;
                self.data_buffer = self.ppu_read(v, cart);
                if v >= 0x3F00 {
                    data = self.ppu_read(v, cart);
                    self.data_buffer = self.ppu_read(v.saturating_sub(0x1000), cart);
                }
                self.increment_vram_addr();
                data
            }
            _ => 0,
        }
    }

    pub fn cpu_write(&mut self, addr: u16, data: u8, cart: &mut Cartridge) {
        match addr {
            0x2000 => {
                let prev_nmi = (self.ctrl & 0x80) != 0;
                self.ctrl = data;
                self.t = (self.t & 0xF3FF) | (((data as u16) & 0x03) << 10);
                let now_nmi = (self.ctrl & 0x80) != 0;
                if !prev_nmi && now_nmi && (self.status & 0x80) != 0 {
                    self.nmi_interrupt = true;
                }
            }
            0x2001 => self.mask = data,
            0x2003 => self.oam_addr = data,
            0x2004 => {
                self.oam_data[self.oam_addr as usize] = data;
                self.oam_addr = self.oam_addr.wrapping_add(1);
            }
            0x2005 => {
                if !self.w {
                    self.t = (self.t & 0xFFE0) | ((data as u16) >> 3);
                    self.x = data & 0x07;
                    self.w = true;
                } else {
                    self.t = (self.t & 0x8FFF) | (((data as u16) & 0x07) << 12);
                    self.t = (self.t & 0xFC1F) | (((data as u16) & 0xF8) << 2);
                    self.w = false;
                }
            }
            0x2006 => {
                if !self.w {
                    self.t = (self.t & 0x00FF) | (((data as u16) & 0x3F) << 8);
                    self.w = true;
                } else {
                    self.t = (self.t & 0xFF00) | (data as u16);
                    self.v = self.t;
                    self.w = false;
                }
            }
            0x2007 => {
                let v = self.v & 0x3FFF;
                self.ppu_write(v, data, cart);
                self.increment_vram_addr();
            }
            _ => {}
        }
    }

    fn increment_vram_addr(&mut self) {
        let inc = if (self.ctrl & 0x04) != 0 { 32 } else { 1 };
        self.v = self.v.wrapping_add(inc);
    }

    fn rendering_enabled(&self) -> bool {
        (self.mask & 0x18) != 0
    }

    fn increment_scroll_x(&mut self) {
        if !self.rendering_enabled() {
            return;
        }
        if (self.v & 0x001F) == 31 {
            self.v &= !0x001F;
            self.v ^= 0x0400;
        } else {
            self.v += 1;
        }
    }

    fn increment_scroll_y(&mut self) {
        if !self.rendering_enabled() {
            return;
        }
        if (self.v & 0x7000) != 0x7000 {
            self.v += 0x1000;
        } else {
            self.v &= !0x7000;
            let mut y = (self.v & 0x03E0) >> 5;
            if y == 29 {
                y = 0;
                self.v ^= 0x0800;
            } else if y == 31 {
                y = 0;
            } else {
                y += 1;
            }
            self.v = (self.v & !0x03E0) | (y << 5);
        }
    }

    fn transfer_address_x(&mut self) {
        if self.rendering_enabled() {
            self.v = (self.v & !0x041F) | (self.t & 0x041F);
        }
    }

    fn transfer_address_y(&mut self) {
        if self.rendering_enabled() {
            self.v = (self.v & !0x7BE0) | (self.t & 0x7BE0);
        }
    }

    fn evaluate_scanline_sprites(&mut self) {
        self.scanline_sprite_count = 0;
        if (self.mask & 0x10) == 0 {
            return;
        }

        let screen_y = self.scanline as usize;
        let sprite_height = if (self.ctrl & 0x20) != 0 { 16 } else { 8 };
        let max_sprites = if self.sprite_limit_enabled { 8 } else { 64 };

        for i in 0..64 {
            let sprite_y = (self.oam_data[i * 4] as usize).wrapping_add(1);
            if screen_y >= sprite_y && screen_y < sprite_y + sprite_height {
                if self.scanline_sprite_count < max_sprites {
                    self.scanline_sprites[self.scanline_sprite_count] = i as u8;
                    self.scanline_sprite_count += 1;
                } else {
                    self.status |= 0x20;
                    break;
                }
            }
        }
    }

    pub fn step(&mut self, cart: &mut Cartridge) {
        if self.scanline >= 0 && self.scanline < 240 {
            if self.cycle == 1 {
                self.evaluate_scanline_sprites();
            }
            if self.cycle >= 1 && self.cycle <= 256 {
                self.render_pixel(cart);
                if self.cycle % 8 == 0 {
                    self.increment_scroll_x();
                }
            }
            if self.cycle == 256 {
                self.increment_scroll_y();
            }
            if self.cycle == 257 {
                self.transfer_address_x();
            }
            if self.cycle == 260 && self.rendering_enabled() {
                cart.clock_scanline_irq();
            }
        }

        if self.scanline == 241 && self.cycle == 1 {
            self.status |= 0x80;
            cart.mmc5_in_frame.set(false);
            if (self.ctrl & 0x80) != 0 {
                self.nmi_interrupt = true;
            }
        }

        if self.scanline == 261 {
            if self.cycle == 1 {
                self.status &= !0xE0;
            }
            if self.cycle == 257 {
                self.transfer_address_x();
            }
            if self.cycle >= 280 && self.cycle <= 304 {
                self.transfer_address_y();
            }
        }

        self.cycle += 1;
        // Pula o ciclo 340 em frames ímpares se a renderização estiver ativa
        let end_cycle = if self.scanline == 261 && self.is_odd_frame && self.rendering_enabled() {
            340
        } else {
            341
        };

        if self.cycle >= end_cycle {
            self.cycle = 0;
            self.scanline += 1;
            if self.scanline > 261 {
                self.scanline = 0;
                self.frame_complete = true;
                self.is_odd_frame = !self.is_odd_frame; // Alterna a cada frame
            }
        }
    }

    fn render_pixel(&mut self, cart: &Cartridge) {
        let screen_x = (self.cycle - 1) as usize;
        let screen_y = self.scanline as usize;

        let mut bg_pixel = 0;
        let mut bg_palette = 0;

        if (self.mask & 0x08) != 0 {
            let fine_x_total = (self.x as usize) + (screen_x % 8);
            let mut temp_v = self.v;
            if fine_x_total >= 8 {
                if (temp_v & 0x001F) == 31 {
                    temp_v &= !0x001F;
                    temp_v ^= 0x0400;
                } else {
                    temp_v += 1;
                }
            }
            let bit_mux = 7 - (fine_x_total % 8);
            let fine_y = (self.v >> 12) & 0x07;

            let tile_addr = 0x2000 | (temp_v & 0x0FFF);
            let tile_id = self.ppu_read(tile_addr, cart) as u16;
            let bg_table = if (self.ctrl & 0x10) != 0 { 0x1000 } else { 0x0000 };
            let pattern_addr = bg_table + tile_id * 16 + fine_y;

            let p_lo = (self.ppu_read(pattern_addr, cart) >> bit_mux) & 1;
            let p_hi = (self.ppu_read(pattern_addr + 8, cart) >> bit_mux) & 1;
            bg_pixel = (p_hi << 1) | p_lo;

            let attr_addr = 0x23C0
                | (temp_v & 0x0C00)
                | ((temp_v >> 4) & 0x38)
                | ((temp_v >> 2) & 0x07);
            let attr_byte = self.ppu_read(attr_addr, cart);
            let shift = ((temp_v >> 4) & 4) | (temp_v & 2);
            bg_palette = (attr_byte >> shift) & 0x03;
        }

        let mut fg_pixel = 0;
        let mut fg_palette = 0;
        let mut fg_priority = false;
        let mut is_sprite_zero = false;

        if (self.mask & 0x10) != 0 && ((self.mask & 0x04) != 0 || screen_x >= 8) {
            let sprite_height = if (self.ctrl & 0x20) != 0 { 16 } else { 8 };
            for sprite_idx in 0..self.scanline_sprite_count {
                let i = self.scanline_sprites[sprite_idx] as usize;
                let base = i * 4;
                let sp_y = (self.oam_data[base] as usize).wrapping_add(1);
                let sp_tile = self.oam_data[base + 1] as u16;
                let sp_attr = self.oam_data[base + 2];
                let sp_x = self.oam_data[base + 3] as usize;

                if screen_y >= sp_y
                    && screen_y < sp_y + sprite_height
                    && screen_x >= sp_x
                    && screen_x < sp_x + 8
                {
                    let flip_h = (sp_attr & 0x40) != 0;
                    let flip_v = (sp_attr & 0x80) != 0;

                    let mut row = screen_y - sp_y;
                    if flip_v {
                        row = (sprite_height - 1) - row;
                    }
                    let col = if flip_h {
                        screen_x - sp_x
                    } else {
                        7 - (screen_x - sp_x)
                    };

                    let pattern_addr = if sprite_height == 8 {
                        let table = if (self.ctrl & 0x08) != 0 { 0x1000 } else { 0x0000 };
                        table + sp_tile * 16 + (row as u16)
                    } else {
                        let table = ((sp_tile & 1) as u16) * 0x1000;
                        let tile_idx = (sp_tile & 0xFE) + if row >= 8 { 1 } else { 0 };
                        table + tile_idx * 16 + ((row % 8) as u16)
                    };

                    let p_lo = (self.ppu_read(pattern_addr, cart) >> col) & 1;
                    let p_hi = (self.ppu_read(pattern_addr + 8, cart) >> col) & 1;
                    let pixel = (p_hi << 1) | p_lo;

                    if pixel != 0 {
                        if i == 0 {
                            is_sprite_zero = true;
                        }
                        fg_pixel = pixel;
                        fg_palette = (sp_attr & 0x03) + 4;
                        fg_priority = (sp_attr & 0x20) == 0;
                        break;
                    }
                }
            }
        }

        if is_sprite_zero
            && screen_x < 255
            && bg_pixel != 0
            && fg_pixel != 0
            && (self.mask & 0x18) == 0x18
            && (((self.mask & 0x06) == 0x06) || screen_x >= 8)
        {
            self.status |= 0x40;
        }

        let (final_pixel, final_palette) = match (bg_pixel, fg_pixel) {
            (0, 0) => (0, 0),
            (0, _) => (fg_pixel, fg_palette),
            (_, 0) => (bg_pixel, bg_palette),
            (_, _) => {
                if fg_priority {
                    (fg_pixel, fg_palette)
                } else {
                    (bg_pixel, bg_palette)
                }
            }
        };

        let color_addr = if final_pixel == 0 {
            0x3F00
        } else {
            0x3F00 | ((final_palette as u16) << 2) | (final_pixel as u16)
        };
        let color_idx = (self.ppu_read(color_addr, cart) & 0x3F) as usize;
        self.framebuffer[screen_y * 256 + screen_x] = SYSTEM_PALETTE[color_idx];
    }
}

// ============================================================================
// BARRAMENTO PRINCIPAL (BUS)
// ============================================================================
#[derive(Clone)]
pub struct Bus {
    pub ram: [u8; 2048],
    pub ppu: Ppu,
    pub apu: Apu,
    pub cart: Cartridge,
    pub joypad1: Joypad,
}

impl Bus {
    pub fn new(cart: Cartridge, apu: Apu) -> Self {
        Self {
            ram: [0; 2048],
            ppu: Ppu::new(),
            apu,
            cart,
            joypad1: Joypad::new(),
        }
    }

    pub fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x1FFF => self.ram[(addr & 0x07FF) as usize],
            0x2000..=0x3FFF => self.ppu.cpu_read(0x2000 + (addr & 0x0007), &self.cart),
            0x4015 => self.apu.cpu_read(addr),
            0x4016 => self.joypad1.read(),
            0x5000..=0xFFFF => self.cart.cpu_read(addr),
            _ => 0,
        }
    }

    pub fn cpu_write(&mut self, addr: u16, data: u8) {
        match addr {
            0x0000..=0x1FFF => self.ram[(addr & 0x07FF) as usize] = data,
            0x2000..=0x3FFF => self.ppu.cpu_write(0x2000 + (addr & 0x0007), data, &mut self.cart),
            0x4000..=0x4013 | 0x4015 | 0x4017 => self.apu.cpu_write(addr, data),
            0x4014 => {
                let base = (data as u16) << 8;
                for i in 0..256 {
                    let b = self.cpu_read(base + i);
                    let oam_addr = self.ppu.oam_addr;
                    self.ppu.oam_data[oam_addr as usize] = b;
                    self.ppu.oam_addr = self.ppu.oam_addr.wrapping_add(1);
                }
                self.tick(513);
            }
            0x4016 => self.joypad1.write(data),
            0x5000..=0xFFFF => self.cart.cpu_write(addr, data),
            _ => {}
        }
    }

    pub fn cpu_read_u16(&mut self, addr: u16) -> u16 {
        let lo = self.cpu_read(addr) as u16;
        let hi = self.cpu_read(addr.wrapping_add(1)) as u16;
        (hi << 8) | lo
    }

    pub fn tick(&mut self, cpu_cycles: usize) {
        for _ in 0..cpu_cycles {
            // Se o DMC pediu leitura da memória da CPU:
            if let Some(addr) = self.apu.dmc.dma_read_addr.take() {
                let byte = self.cpu_read(addr);
                self.apu.dmc.sample_buffer = Some(byte);
            }

            if self.cart.mapper == 69 && self.cart.fme7_irq_counter_enable {
                if self.cart.fme7_irq_counter == 0 {
                    self.cart.fme7_irq_counter = 0xFFFF;
                    if self.cart.fme7_irq_enabled { self.cart.mmc3_irq_pending = true; }
                } else {
                    self.cart.fme7_irq_counter -= 1;
                }
            }

            // VRC IRQ no modo ciclo de CPU (VRC4)
            if matches!(self.cart.mapper, 21 | 22 | 23 | 25) && self.cart.vrc_irq_enabled && self.cart.vrc_irq_cycle_mode {
                self.cart.vrc_irq_prescaler -= 3;
                if self.cart.vrc_irq_prescaler <= 0 {
                    self.cart.vrc_irq_prescaler += 341;
                    self.cart.clock_vrc_irq();
                }
            }

            self.apu.step_cpu_cycle();
            for _ in 0..3 {
                self.ppu.step(&mut self.cart);
            }
        }
    }
}

// ============================================================================
// CPU RICOH 2A03 (MOS 6502)
// ============================================================================
#[derive(Clone, Copy, PartialEq)]
enum AddrMode {
    Imp, Acc, Imm, Zp0, Zpx, Zpy, Rel, Abs, Abx, Aby, Ind, Izx, Izy,
}

const FLAG_C: u8 = 1 << 0;
const FLAG_Z: u8 = 1 << 1;
const FLAG_I: u8 = 1 << 2;
const FLAG_D: u8 = 1 << 3;
const FLAG_B: u8 = 1 << 4;
const FLAG_U: u8 = 1 << 5;
const FLAG_V: u8 = 1 << 6;
const FLAG_N: u8 = 1 << 7;

#[derive(Clone)]
pub struct Cpu {
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub sp: u8,
    pub pc: u16,
    pub status: u8,
    pub bus: Bus,
}

impl Cpu {
    pub fn new(bus: Bus) -> Self {
        Self {
            a: 0,
            x: 0,
            y: 0,
            sp: 0xFD,
            pc: 0,
            status: FLAG_I | FLAG_U,
            bus,
        }
    }

    pub fn reset(&mut self) {
        self.a = 0;
        self.x = 0;
        self.y = 0;
        self.sp = 0xFD;
        self.status = FLAG_I | FLAG_U;
        self.pc = self.bus.cpu_read_u16(0xFFFC);
        self.bus.tick(7);
    }

    fn set_flag(&mut self, flag: u8, cond: bool) {
        if cond { self.status |= flag; } else { self.status &= !flag; }
    }

    fn get_flag(&self, flag: u8) -> bool {
        (self.status & flag) != 0
    }

    fn update_zn(&mut self, val: u8) {
        self.set_flag(FLAG_Z, val == 0);
        self.set_flag(FLAG_N, (val & 0x80) != 0);
    }

    fn push(&mut self, data: u8) {
        self.bus.cpu_write(0x0100 | (self.sp as u16), data);
        self.sp = self.sp.wrapping_sub(1);
    }

    fn pop(&mut self) -> u8 {
        self.sp = self.sp.wrapping_add(1);
        self.bus.cpu_read(0x0100 | (self.sp as u16))
    }

    fn push_u16(&mut self, data: u16) {
        self.push((data >> 8) as u8);
        self.push((data & 0xFF) as u8);
    }

    fn pop_u16(&mut self) -> u16 {
        let lo = self.pop() as u16;
        let hi = self.pop() as u16;
        (hi << 8) | lo
    }

    fn nmi(&mut self) {
        self.push_u16(self.pc);
        self.set_flag(FLAG_B, false);
        self.set_flag(FLAG_U, true);
        self.push(self.status);
        self.set_flag(FLAG_I, true);
        self.pc = self.bus.cpu_read_u16(0xFFFA);
        self.bus.tick(7);
    }

    fn irq(&mut self) {
        if !self.get_flag(FLAG_I) {
            self.push_u16(self.pc);
            self.set_flag(FLAG_B, false);
            self.set_flag(FLAG_U, true);
            self.push(self.status);
            self.set_flag(FLAG_I, true);
            self.pc = self.bus.cpu_read_u16(0xFFFE);
            self.bus.tick(7);
        }
    }

    fn fetch_operand_addr(&mut self, mode: AddrMode) -> (u16, bool) {
        match mode {
            AddrMode::Imp | AddrMode::Acc => (0, false),
            AddrMode::Imm => {
                let addr = self.pc;
                self.pc = self.pc.wrapping_add(1);
                (addr, false)
            }
            AddrMode::Zp0 => {
                let addr = self.bus.cpu_read(self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                (addr, false)
            }
            AddrMode::Zpx => {
                let base = self.bus.cpu_read(self.pc);
                self.pc = self.pc.wrapping_add(1);
                (base.wrapping_add(self.x) as u16, false)
            }
            AddrMode::Zpy => {
                let base = self.bus.cpu_read(self.pc);
                self.pc = self.pc.wrapping_add(1);
                (base.wrapping_add(self.y) as u16, false)
            }
            AddrMode::Rel => {
                let offset = self.bus.cpu_read(self.pc) as i8;
                self.pc = self.pc.wrapping_add(1);
                let addr = self.pc.wrapping_add(offset as u16);
                (addr, (self.pc & 0xFF00) != (addr & 0xFF00))
            }
            AddrMode::Abs => {
                let addr = self.bus.cpu_read_u16(self.pc);
                self.pc = self.pc.wrapping_add(2);
                (addr, false)
            }
            AddrMode::Abx => {
                let base = self.bus.cpu_read_u16(self.pc);
                self.pc = self.pc.wrapping_add(2);
                let addr = base.wrapping_add(self.x as u16);
                (addr, (base & 0xFF00) != (addr & 0xFF00))
            }
            AddrMode::Aby => {
                let base = self.bus.cpu_read_u16(self.pc);
                self.pc = self.pc.wrapping_add(2);
                let addr = base.wrapping_add(self.y as u16);
                (addr, (base & 0xFF00) != (addr & 0xFF00))
            }
            AddrMode::Ind => {
                // Lê o ponteiro de 16 bits (endereço onde está o destino)
                let ptr = self.bus.cpu_read_u16(self.pc);
                self.pc = self.pc.wrapping_add(2);

                let lo = self.bus.cpu_read(ptr) as u16;

                // BUG ORIGINAL DO 6502: Se ptr termina em 0xFF (ex: $02FF),
                // o byte alto é lido do início da MESMA página ($0200) e não da próxima ($0300).
                let hi_addr = if (ptr & 0x00FF) == 0x00FF {
                    ptr & 0xFF00
                } else {
                    ptr + 1
                };

                let hi = self.bus.cpu_read(hi_addr) as u16;
                ((hi << 8) | lo, false)
            }
            AddrMode::Izx => {
                let base = self.bus.cpu_read(self.pc);
                self.pc = self.pc.wrapping_add(1);
                let ptr = base.wrapping_add(self.x);
                let lo = self.bus.cpu_read(ptr as u16) as u16;
                let hi = self.bus.cpu_read(ptr.wrapping_add(1) as u16) as u16;
                ((hi << 8) | lo, false)
            }
            AddrMode::Izy => {
                let ptr = self.bus.cpu_read(self.pc);
                self.pc = self.pc.wrapping_add(1);
                let lo = self.bus.cpu_read(ptr as u16) as u16;
                let hi = self.bus.cpu_read(ptr.wrapping_add(1) as u16) as u16;
                let base = (hi << 8) | lo;
                let addr = base.wrapping_add(self.y as u16);
                (addr, (base & 0xFF00) != (addr & 0xFF00))
            }
        }
    }

    fn adc(&mut self, val: u8) {
        let c = if self.get_flag(FLAG_C) { 1u16 } else { 0u16 };
        let sum = (self.a as u16) + (val as u16) + c;
        self.set_flag(FLAG_C, sum > 0xFF);
        let res = sum as u8;
        self.set_flag(FLAG_V, ((!(self.a ^ val) & (self.a ^ res)) & 0x80) != 0);
        self.a = res;
        self.update_zn(self.a);
    }

    fn sbc(&mut self, val: u8) {
        let c = if self.get_flag(FLAG_C) { 0u16 } else { 1u16 };
        let sum = (self.a as u16).wrapping_sub(val as u16).wrapping_sub(c);
        self.set_flag(FLAG_C, sum < 0x100);
        self.set_flag(FLAG_V, ((self.a ^ val) & (self.a ^ (sum as u8)) & 0x80) != 0);
        self.a = sum as u8;
        self.update_zn(self.a);
    }

    fn branch(&mut self, cond: bool, addr: u16, _crossed: bool) -> usize {
        if cond {
            let old_pc = self.pc;
            self.pc = addr;
            let page_crossed = (old_pc & 0xFF00) != (self.pc & 0xFF00);
            if page_crossed {
                2
            } else {
                1
            }
        } else {
            0
        }
    }

    pub fn step(&mut self) {
        if self.bus.ppu.nmi_interrupt {
            self.bus.ppu.nmi_interrupt = false;
            self.nmi();
        } else if (self.bus.cart.mmc3_irq_pending || self.bus.apu.dmc.irq_pending) && !self.get_flag(FLAG_I) {
            self.bus.cart.mmc3_irq_pending = false;
            self.irq();
        }

        let opcode = self.bus.cpu_read(self.pc);
        self.pc = self.pc.wrapping_add(1);

        let (op, mode, mut cycles) = decode_opcode(opcode);
        let (addr, page_crossed) = self.fetch_operand_addr(mode);

        match op {
            "LDA" => { self.a = self.bus.cpu_read(addr); self.update_zn(self.a); if page_crossed { cycles += 1; } }
            "LDX" => { self.x = self.bus.cpu_read(addr); self.update_zn(self.x); if page_crossed { cycles += 1; } }
            "LDY" => { self.y = self.bus.cpu_read(addr); self.update_zn(self.y); if page_crossed { cycles += 1; } }
            "LAX" => {
                let val = self.bus.cpu_read(addr);
                self.a = val;
                self.x = val;
                self.update_zn(val);
                if page_crossed { cycles += 1; }
            }
            "STA" => self.bus.cpu_write(addr, self.a),
            "STX" => self.bus.cpu_write(addr, self.x),
            "STY" => self.bus.cpu_write(addr, self.y),
            "SAX" => self.bus.cpu_write(addr, self.a & self.x),
            "TAX" => { self.x = self.a; self.update_zn(self.x); }
            "TAY" => { self.y = self.a; self.update_zn(self.y); }
            "TXA" => { self.a = self.x; self.update_zn(self.a); }
            "TYA" => { self.a = self.y; self.update_zn(self.a); }
            "TSX" => { self.x = self.sp; self.update_zn(self.x); }
            "TXS" => self.sp = self.x,
            "PHA" => self.push(self.a),
            "PHP" => self.push(self.status | FLAG_B | FLAG_U),
            "PLA" => { self.a = self.pop(); self.update_zn(self.a); }
            "PLP" => self.status = (self.pop() & !FLAG_B) | FLAG_U,
            "AND" => { self.a &= self.bus.cpu_read(addr); self.update_zn(self.a); if page_crossed { cycles += 1; } }
            "EOR" => { self.a ^= self.bus.cpu_read(addr); self.update_zn(self.a); if page_crossed { cycles += 1; } }
            "ORA" => { self.a |= self.bus.cpu_read(addr); self.update_zn(self.a); if page_crossed { cycles += 1; } }
            "BIT" => {
                let val = self.bus.cpu_read(addr);
                self.set_flag(FLAG_Z, (self.a & val) == 0);
                self.set_flag(FLAG_V, (val & 0x40) != 0);
                self.set_flag(FLAG_N, (val & 0x80) != 0);
            }
            "ADC" => { let val = self.bus.cpu_read(addr); self.adc(val); if page_crossed { cycles += 1; } }
            "SBC" => {
                let val = self.bus.cpu_read(addr);
                self.sbc(val);
                if page_crossed {
                    cycles += 1;
                }
            }
            "DCP" => {
                let val = self.bus.cpu_read(addr).wrapping_sub(1);
                self.bus.cpu_write(addr, val);
                self.set_flag(FLAG_C, self.a >= val);
                self.update_zn(self.a.wrapping_sub(val));
            }
            "ISB" => {
                let val = self.bus.cpu_read(addr).wrapping_add(1);
                self.bus.cpu_write(addr, val);
                self.sbc(val);
            }
            "SLO" => {
                let val = self.bus.cpu_read(addr);
                self.set_flag(FLAG_C, (val & 0x80) != 0);
                let result = val << 1;
                self.bus.cpu_write(addr, result);
                self.a |= result;
                self.update_zn(self.a);
            }
            "RLA" => {
                let carry_in = if self.get_flag(FLAG_C) { 1 } else { 0 };
                let val = self.bus.cpu_read(addr);
                self.set_flag(FLAG_C, (val & 0x80) != 0);
                let result = (val << 1) | carry_in;
                self.bus.cpu_write(addr, result);
                self.a &= result;
                self.update_zn(self.a);
            }
            "SRE" => {
                let val = self.bus.cpu_read(addr);
                self.set_flag(FLAG_C, (val & 1) != 0);
                let result = val >> 1;
                self.bus.cpu_write(addr, result);
                self.a ^= result;
                self.update_zn(self.a);
            }
            "RRA" => {
                let carry_in = if self.get_flag(FLAG_C) { 0x80 } else { 0 };
                let val = self.bus.cpu_read(addr);
                self.set_flag(FLAG_C, (val & 1) != 0);
                let result = (val >> 1) | carry_in;
                self.bus.cpu_write(addr, result);
                self.adc(result);
            }
            "ANC" => {
                self.a &= self.bus.cpu_read(addr);
                self.update_zn(self.a);
                self.set_flag(FLAG_C, (self.a & 0x80) != 0);
            }
            "ALR" => {
                self.a &= self.bus.cpu_read(addr);
                self.set_flag(FLAG_C, (self.a & 1) != 0);
                self.a >>= 1;
                self.update_zn(self.a);
            }
            "ARR" => {
                self.a &= self.bus.cpu_read(addr);
                let carry_in = if self.get_flag(FLAG_C) { 0x80 } else { 0 };
                self.a = (self.a >> 1) | carry_in;
                self.update_zn(self.a);
                let bit_6 = (self.a >> 6) & 1;
                let bit_5 = (self.a >> 5) & 1;
                self.set_flag(FLAG_C, bit_6 != 0);
                self.set_flag(FLAG_V, (bit_6 ^ bit_5) != 0);
            }
            "AXS" => {
                let val = self.bus.cpu_read(addr);
                let ax = self.a & self.x;
                self.set_flag(FLAG_C, ax >= val);
                self.x = ax.wrapping_sub(val);
                self.update_zn(self.x);
            }
            "XAA" => {
                self.a = (self.a | 0xEE) & self.x & self.bus.cpu_read(addr);
                self.update_zn(self.a);
            }
            "LAS" => {
                let val = self.bus.cpu_read(addr) & self.sp;
                self.a = val; self.x = val; self.sp = val;
                self.update_zn(val);
                if page_crossed { cycles += 1; }
            }
            "AHX" => {
                let high = ((addr >> 8) as u8).wrapping_add(1);
                self.bus.cpu_write(addr, self.a & self.x & high);
            }
            "SHY" => {
                let high = ((addr >> 8) as u8).wrapping_add(1);
                self.bus.cpu_write(addr, self.y & high);
            }
            "SHX" => {
                let high = ((addr >> 8) as u8).wrapping_add(1);
                self.bus.cpu_write(addr, self.x & high);
            }
            "TAS" => {
                self.sp = self.a & self.x;
                let high = ((addr >> 8) as u8).wrapping_add(1);
                self.bus.cpu_write(addr, self.sp & high);
            }
            "NOP" => {
                if page_crossed { cycles += 1; }
            }
            "CMP" => {
                let val = self.bus.cpu_read(addr);
                self.set_flag(FLAG_C, self.a >= val);
                self.update_zn(self.a.wrapping_sub(val));
                if page_crossed { cycles += 1; }
            }
            "CPX" => {
                let val = self.bus.cpu_read(addr);
                self.set_flag(FLAG_C, self.x >= val);
                self.update_zn(self.x.wrapping_sub(val));
            }
            "CPY" => {
                let val = self.bus.cpu_read(addr);
                self.set_flag(FLAG_C, self.y >= val);
                self.update_zn(self.y.wrapping_sub(val));
            }
            "INC" => {
                let val = self.bus.cpu_read(addr).wrapping_add(1);
                self.bus.cpu_write(addr, val);
                self.update_zn(val);
            }
            "INX" => { self.x = self.x.wrapping_add(1); self.update_zn(self.x); }
            "INY" => { self.y = self.y.wrapping_add(1); self.update_zn(self.y); }
            "DEC" => {
                let val = self.bus.cpu_read(addr).wrapping_sub(1);
                self.bus.cpu_write(addr, val);
                self.update_zn(val);
            }
            "DEX" => { self.x = self.x.wrapping_sub(1); self.update_zn(self.x); }
            "DEY" => { self.y = self.y.wrapping_sub(1); self.update_zn(self.y); }
            "ASL" => {
                if mode == AddrMode::Acc {
                    self.set_flag(FLAG_C, (self.a & 0x80) != 0);
                    self.a <<= 1;
                    self.update_zn(self.a);
                } else {
                    let val = self.bus.cpu_read(addr);
                    self.set_flag(FLAG_C, (val & 0x80) != 0);
                    let res = val << 1;
                    self.bus.cpu_write(addr, res);
                    self.update_zn(res);
                }
            }
            "LSR" => {
                if mode == AddrMode::Acc {
                    self.set_flag(FLAG_C, (self.a & 1) != 0);
                    self.a >>= 1;
                    self.update_zn(self.a);
                } else {
                    let val = self.bus.cpu_read(addr);
                    self.set_flag(FLAG_C, (val & 1) != 0);
                    let res = val >> 1;
                    self.bus.cpu_write(addr, res);
                    self.update_zn(res);
                }
            }
            "ROL" => {
                let c = if self.get_flag(FLAG_C) { 1 } else { 0 };
                if mode == AddrMode::Acc {
                    self.set_flag(FLAG_C, (self.a & 0x80) != 0);
                    self.a = (self.a << 1) | c;
                    self.update_zn(self.a);
                } else {
                    let val = self.bus.cpu_read(addr);
                    self.set_flag(FLAG_C, (val & 0x80) != 0);
                    let res = (val << 1) | c;
                    self.bus.cpu_write(addr, res);
                    self.update_zn(res);
                }
            }
            "ROR" => {
                let c = if self.get_flag(FLAG_C) { 0x80 } else { 0 };
                if mode == AddrMode::Acc {
                    self.set_flag(FLAG_C, (self.a & 1) != 0);
                    self.a = (self.a >> 1) | c;
                    self.update_zn(self.a);
                } else {
                    let val = self.bus.cpu_read(addr);
                    self.set_flag(FLAG_C, (val & 1) != 0);
                    let res = (val >> 1) | c;
                    self.bus.cpu_write(addr, res);
                    self.update_zn(res);
                }
            }
            "JMP" => self.pc = addr,
            "JSR" => { self.push_u16(self.pc.wrapping_sub(1)); self.pc = addr; }
            "RTS" => self.pc = self.pop_u16().wrapping_add(1),
            "RTI" => { self.bus.apu.dmc.irq_pending = false; self.status = (self.pop() & !FLAG_B) | FLAG_U; self.pc = self.pop_u16(); }
            "BCC" => cycles += self.branch(!self.get_flag(FLAG_C), addr, page_crossed),
            "BCS" => cycles += self.branch(self.get_flag(FLAG_C), addr, page_crossed),
            "BEQ" => cycles += self.branch(self.get_flag(FLAG_Z), addr, page_crossed),
            "BMI" => cycles += self.branch(self.get_flag(FLAG_N), addr, page_crossed),
            "BNE" => cycles += self.branch(!self.get_flag(FLAG_Z), addr, page_crossed),
            "BPL" => cycles += self.branch(!self.get_flag(FLAG_N), addr, page_crossed),
            "BVC" => cycles += self.branch(!self.get_flag(FLAG_V), addr, page_crossed),
            "BVS" => cycles += self.branch(self.get_flag(FLAG_V), addr, page_crossed),
            "CLC" => self.set_flag(FLAG_C, false),
            "CLD" => self.set_flag(FLAG_D, false),
            "CLI" => self.set_flag(FLAG_I, false),
            "CLV" => self.set_flag(FLAG_V, false),
            "SEC" => self.set_flag(FLAG_C, true),
            "SED" => self.set_flag(FLAG_D, true),
            "SEI" => self.set_flag(FLAG_I, true),
            "BRK" => {
                self.pc = self.pc.wrapping_add(1);
                self.push_u16(self.pc);
                self.push(self.status | FLAG_B | FLAG_U);
                self.set_flag(FLAG_I, true);
                self.pc = self.bus.cpu_read_u16(0xFFFE);
            }
            _ => {}
        }

        self.bus.tick(cycles);
    }
}

fn get_save_state_path(rom_path: &str, slot: usize) -> PathBuf {
    let dir = PathBuf::from(".states");
    let _ = fs::create_dir_all(&dir);
    let stem = std::path::Path::new(rom_path).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "game".to_string());
    dir.join(format!("{}.st{}", sanitize_rom_name_for_scraping(&stem).replace(' ', "_"), slot))
}

fn has_save_state_on_disk(rom_path: &str, slot: usize) -> bool { get_save_state_path(rom_path, slot).is_file() }

// Snapshots rápidos de rewind ficam em RAM; os save states permanentes usam arquivo.
pub struct RewindManager { snapshots: VecDeque<Cpu>, frame_counter: usize }
impl RewindManager {
    fn new() -> Self { Self { snapshots: VecDeque::with_capacity(300), frame_counter: 0 } }
    fn clear(&mut self) { self.snapshots.clear(); self.frame_counter = 0; }
    fn record_frame(&mut self, cpu: &Cpu) {
        self.frame_counter = self.frame_counter.wrapping_add(1);
        if self.frame_counter % 2 == 0 {
            if self.snapshots.len() >= 300 { self.snapshots.pop_front(); }
            self.snapshots.push_back(cpu.clone());
        }
    }
    fn pop_rewind_frame(&mut self) -> Option<Cpu> { self.snapshots.pop_back() }
}

impl Cpu {
    // Formato v1 persistente: CPU, RAM, estado da PPU e registradores dos mappers.
    // O áudio é reinicializado pelo emulador após carregar, pois sua saída é um stream vivo.
    fn save_state_to_disk(&self, rom_path: &str, slot: usize) -> bool {
        let mut b = Vec::with_capacity(16_000);
        b.extend_from_slice(b"RNES_ST2");
        b.extend_from_slice(&self.a.to_le_bytes()); b.push(self.x); b.push(self.y); b.push(self.sp);
        b.extend_from_slice(&self.pc.to_le_bytes()); b.push(self.status);
        b.extend_from_slice(&self.bus.ram);
        let ppu = &self.bus.ppu;
        b.extend_from_slice(&ppu.vram); b.extend_from_slice(&ppu.palette_table); b.extend_from_slice(&ppu.oam_data);
        b.extend_from_slice(&[ppu.ctrl, ppu.mask, ppu.status, ppu.oam_addr]);
        b.extend_from_slice(&ppu.v.to_le_bytes()); b.extend_from_slice(&ppu.t.to_le_bytes());
        b.extend_from_slice(&[ppu.x, ppu.w as u8, ppu.data_buffer]);
        b.extend_from_slice(&ppu.scanline.to_le_bytes()); b.extend_from_slice(&(ppu.cycle as u64).to_le_bytes());
        b.extend_from_slice(&[ppu.nmi_interrupt as u8, ppu.frame_complete as u8, ppu.is_odd_frame as u8, ppu.sprite_limit_enabled as u8]);
        let c = &self.bus.cart;
        b.extend_from_slice(&(c.prg_ram.len() as u32).to_le_bytes());
        b.extend_from_slice(&c.prg_ram);
        b.push(match c.mirroring { Mirroring::Horizontal=>0, Mirroring::Vertical=>1, Mirroring::OneScreenLo=>2, Mirroring::OneScreenHi=>3, Mirroring::FourScreen=>4 });
        for n in [c.prg_bank,c.chr_bank_0,c.chr_bank_1,c.mmc2_prg_bank] { b.extend_from_slice(&(n as u64).to_le_bytes()); }
        b.extend_from_slice(&[c.mmc1_shift,c.mmc1_ctrl,c.mmc1_prg_bank,c.mmc3_bank_select]);
        for n in c.mmc3_registers { b.extend_from_slice(&(n as u64).to_le_bytes()); }
        b.extend_from_slice(&[c.mmc3_prg_mode as u8,c.mmc3_chr_mode as u8,c.mmc3_irq_latch,c.mmc3_irq_counter,c.mmc3_irq_enabled as u8,c.mmc3_irq_reload as u8,c.mmc3_irq_pending as u8]);
        b.extend_from_slice(&[c.mmc2_latch_0.get(),c.mmc2_latch_1.get()]);
        for n in c.mmc2_chr_banks { b.extend_from_slice(&(n as u64).to_le_bytes()); }
        for n in c.vrc_prg_banks { b.extend_from_slice(&(n as u64).to_le_bytes()); }
        for n in c.vrc_chr_banks { b.extend_from_slice(&(n as u64).to_le_bytes()); }
        b.extend_from_slice(&[c.vrc_irq_latch,c.vrc_irq_counter,c.vrc_irq_enabled as u8,c.vrc_irq_cycle_mode as u8]);
        b.extend_from_slice(&c.vrc_irq_prescaler.to_le_bytes());
        b.extend_from_slice(&[c.fme7_command,c.fme7_prg_ram_select as u8,c.fme7_prg_ram_enable as u8,c.fme7_irq_enabled as u8,c.fme7_irq_counter_enable as u8]);
        b.extend_from_slice(&c.fme7_irq_counter.to_le_bytes());
        for n in c.fme7_prg_banks { b.extend_from_slice(&(n as u64).to_le_bytes()); }
        for n in c.fme7_chr_banks { b.extend_from_slice(&(n as u64).to_le_bytes()); }
        b.extend_from_slice(&[c.mmc5_prg_mode,c.mmc5_chr_mode,c.mmc5_last_chr_set,c.mmc5_exram_mode,c.mmc5_mult_a,c.mmc5_mult_b,c.mmc5_irq_target,c.mmc5_irq_enabled as u8,c.mmc5_irq_pending.get() as u8,c.mmc5_in_frame.get() as u8,c.mmc5_scanline,c.mmc5_fill_tile,c.mmc5_fill_attr]);
        for n in c.mmc5_prg_banks { b.extend_from_slice(&(n as u64).to_le_bytes()); }
        for n in c.mmc5_chr_banks { b.extend_from_slice(&(n as u64).to_le_bytes()); }
        b.extend_from_slice(&(c.mmc5_chr_upper as u64).to_le_bytes());
        b.extend_from_slice(&(c.mmc5_exram.len() as u32).to_le_bytes()); b.extend_from_slice(&c.mmc5_exram);
        if c.is_chr_ram { b.extend_from_slice(&(c.chr_mem.len() as u32).to_le_bytes()); b.extend_from_slice(&c.chr_mem); } else { b.extend_from_slice(&0u32.to_le_bytes()); }
        fs::write(get_save_state_path(rom_path, slot), b).is_ok()
    }

    fn load_state_from_disk(&mut self, rom_path: &str, slot: usize) -> bool {
        let Ok(b) = fs::read(get_save_state_path(rom_path, slot)) else { return false };
        // Validate the fixed prefix before mutating any emulator memory.
        if b.len() < 12_600 || b.get(..8) != Some(b"RNES_ST2") { return false; }
        let mut p=8usize;
        macro_rules! take { ($n:expr) => {{ let n=$n; if p.checked_add(n).map_or(true, |end| end>b.len()) { return false; } let s=&b[p..p+n]; p+=n; s }} }
        macro_rules! u8v { () => { take!(1)[0] }; }
        macro_rules! u16v { () => { u16::from_le_bytes(take!(2).try_into().unwrap()) }; }
        macro_rules! u32v { () => { u32::from_le_bytes(take!(4).try_into().unwrap()) }; }
        macro_rules! u64v { () => { u64::from_le_bytes(take!(8).try_into().unwrap()) }; }
        self.a=u8v!(); self.x=u8v!(); self.y=u8v!(); self.sp=u8v!(); self.pc=u16v!(); self.status=u8v!();
        self.bus.ram.copy_from_slice(take!(2048));
        let ppu=&mut self.bus.ppu;
        ppu.vram.copy_from_slice(take!(2048)); ppu.palette_table.copy_from_slice(take!(32)); ppu.oam_data.copy_from_slice(take!(256));
        ppu.ctrl=u8v!(); ppu.mask=u8v!(); ppu.status=u8v!(); ppu.oam_addr=u8v!(); ppu.v=u16v!(); ppu.t=u16v!();
        ppu.x=u8v!(); ppu.w=u8v!()!=0; ppu.data_buffer=u8v!(); ppu.scanline=i16::from_le_bytes(take!(2).try_into().unwrap()); ppu.cycle=u64v!() as usize;
        ppu.nmi_interrupt=u8v!()!=0; ppu.frame_complete=u8v!()!=0; ppu.is_odd_frame=u8v!()!=0; ppu.sprite_limit_enabled=u8v!()!=0;
        let c=&mut self.bus.cart;
        let ram_len=u32v!() as usize;
        if ram_len > 1_048_576 { return false; }
        let ram=take!(ram_len);
        let ram_copy=ram.len().min(c.prg_ram.len()); c.prg_ram[..ram_copy].copy_from_slice(&ram[..ram_copy]);
        c.mirroring=match u8v!(){0=>Mirroring::Horizontal,1=>Mirroring::Vertical,2=>Mirroring::OneScreenLo,3=>Mirroring::OneScreenHi,_=>Mirroring::FourScreen};
        c.prg_bank=u64v!() as usize; c.chr_bank_0=u64v!() as usize; c.chr_bank_1=u64v!() as usize; c.mmc2_prg_bank=u64v!() as usize;
        c.mmc1_shift=u8v!(); c.mmc1_ctrl=u8v!(); c.mmc1_prg_bank=u8v!(); c.mmc3_bank_select=u8v!();
        for n in &mut c.mmc3_registers { *n=u64v!() as usize; }
        c.mmc3_prg_mode=u8v!()!=0; c.mmc3_chr_mode=u8v!()!=0; c.mmc3_irq_latch=u8v!(); c.mmc3_irq_counter=u8v!(); c.mmc3_irq_enabled=u8v!()!=0; c.mmc3_irq_reload=u8v!()!=0; c.mmc3_irq_pending=u8v!()!=0;
        c.mmc2_latch_0.set(u8v!()); c.mmc2_latch_1.set(u8v!());
        for n in &mut c.mmc2_chr_banks { *n=u64v!() as usize; } for n in &mut c.vrc_prg_banks { *n=u64v!() as usize; } for n in &mut c.vrc_chr_banks { *n=u64v!() as usize; }
        c.vrc_irq_latch=u8v!(); c.vrc_irq_counter=u8v!(); c.vrc_irq_enabled=u8v!()!=0; c.vrc_irq_cycle_mode=u8v!()!=0; c.vrc_irq_prescaler=i16::from_le_bytes(take!(2).try_into().unwrap());
        c.fme7_command=u8v!(); c.fme7_prg_ram_select=u8v!()!=0; c.fme7_prg_ram_enable=u8v!()!=0; c.fme7_irq_enabled=u8v!()!=0; c.fme7_irq_counter_enable=u8v!()!=0; c.fme7_irq_counter=u16v!();
        for n in &mut c.fme7_prg_banks { *n=u64v!() as usize; } for n in &mut c.fme7_chr_banks { *n=u64v!() as usize; }
        c.mmc5_prg_mode=u8v!(); c.mmc5_chr_mode=u8v!(); c.mmc5_last_chr_set=u8v!(); c.mmc5_exram_mode=u8v!(); c.mmc5_mult_a=u8v!(); c.mmc5_mult_b=u8v!(); c.mmc5_irq_target=u8v!(); c.mmc5_irq_enabled=u8v!()!=0; c.mmc5_irq_pending.set(u8v!()!=0); c.mmc5_in_frame.set(u8v!()!=0); c.mmc5_scanline=u8v!(); c.mmc5_fill_tile=u8v!(); c.mmc5_fill_attr=u8v!();
        for n in &mut c.mmc5_prg_banks { *n=u64v!() as usize; } for n in &mut c.mmc5_chr_banks { *n=u64v!() as usize; }
        c.mmc5_chr_upper=u64v!() as usize;
        let exram_len=u32v!() as usize; if exram_len > 65_536 { return false; } let exram=take!(exram_len); let exram_copy=exram.len().min(c.mmc5_exram.len()); c.mmc5_exram[..exram_copy].copy_from_slice(&exram[..exram_copy]);
        let chr_len=u32v!() as usize;
        if chr_len > 0 {
            if p.checked_add(chr_len).map_or(true, |end| end > b.len()) { return false; }
            if c.is_chr_ram { let n=chr_len.min(c.chr_mem.len()); c.chr_mem[..n].copy_from_slice(&b[p..p+n]); }
        }
        true
    }
}

fn decode_opcode(op: u8) -> (&'static str, AddrMode, usize) {
    use AddrMode::*;
    match op {
        0x00 => ("BRK", Imp, 7), 0x01 => ("ORA", Izx, 6), 0x05 => ("ORA", Zp0, 3),
        0x06 => ("ASL", Zp0, 5), 0x08 => ("PHP", Imp, 3), 0x09 => ("ORA", Imm, 2),
        0x0A => ("ASL", Acc, 2), 0x0D => ("ORA", Abs, 4), 0x0E => ("ASL", Abs, 6),
        0x10 => ("BPL", Rel, 2), 0x11 => ("ORA", Izy, 5), 0x15 => ("ORA", Zpx, 4),
        0x16 => ("ASL", Zpx, 6), 0x18 => ("CLC", Imp, 2), 0x19 => ("ORA", Aby, 4),
        0x1D => ("ORA", Abx, 4), 0x1E => ("ASL", Abx, 7), 0x20 => ("JSR", Abs, 6),
        0x21 => ("AND", Izx, 6), 0x24 => ("BIT", Zp0, 3), 0x25 => ("AND", Zp0, 3),
        0x26 => ("ROL", Zp0, 5), 0x28 => ("PLP", Imp, 4), 0x29 => ("AND", Imm, 2),
        0x2A => ("ROL", Acc, 2), 0x2C => ("BIT", Abs, 4), 0x2D => ("AND", Abs, 4),
        0x2E => ("ROL", Abs, 6), 0x30 => ("BMI", Rel, 2), 0x31 => ("AND", Izy, 5),
        0x35 => ("AND", Zpx, 4), 0x36 => ("ROL", Zpx, 6), 0x38 => ("SEC", Imp, 2),
        0x39 => ("AND", Aby, 4), 0x3D => ("AND", Abx, 4), 0x3E => ("ROL", Abx, 7),
        0x40 => ("RTI", Imp, 6), 0x41 => ("EOR", Izx, 6), 0x45 => ("EOR", Zp0, 3),
        0x46 => ("LSR", Zp0, 5), 0x48 => ("PHA", Imp, 3), 0x49 => ("EOR", Imm, 2),
        0x4A => ("LSR", Acc, 2), 0x4C => ("JMP", Abs, 3), 0x4D => ("EOR", Abs, 4),
        0x4E => ("LSR", Abs, 6), 0x50 => ("BVC", Rel, 2), 0x51 => ("EOR", Izy, 5),
        0x55 => ("EOR", Zpx, 4), 0x56 => ("LSR", Zpx, 6), 0x58 => ("CLI", Imp, 2),
        0x59 => ("EOR", Aby, 4), 0x5D => ("EOR", Abx, 4), 0x5E => ("LSR", Abx, 7),
        0x60 => ("RTS", Imp, 6), 0x61 => ("ADC", Izx, 6), 0x65 => ("ADC", Zp0, 3),
        0x66 => ("ROR", Zp0, 5), 0x68 => ("PLA", Imp, 4), 0x69 => ("ADC", Imm, 2),
        0x6A => ("ROR", Acc, 2), 0x6C => ("JMP", Ind, 5), 0x6D => ("ADC", Abs, 4),
        0x6E => ("ROR", Abs, 6), 0x70 => ("BVS", Rel, 2), 0x71 => ("ADC", Izy, 5),
        0x75 => ("ADC", Zpx, 4), 0x76 => ("ROR", Zpx, 6), 0x78 => ("SEI", Imp, 2),
        0x79 => ("ADC", Aby, 4), 0x7D => ("ADC", Abx, 4), 0x7E => ("ROR", Abx, 7),
        0x81 => ("STA", Izx, 6), 0x84 => ("STY", Zp0, 3), 0x85 => ("STA", Zp0, 3),
        0x86 => ("STX", Zp0, 3), 0x88 => ("DEY", Imp, 2), 0x8A => ("TXA", Imp, 2),
        0x8C => ("STY", Abs, 4), 0x8D => ("STA", Abs, 4), 0x8E => ("STX", Abs, 4),
        0x90 => ("BCC", Rel, 2), 0x91 => ("STA", Izy, 6), 0x94 => ("STY", Zpx, 4),
        0x95 => ("STA", Zpx, 4), 0x96 => ("STX", Zpy, 4), 0x98 => ("TYA", Imp, 2),
        0x99 => ("STA", Aby, 5), 0x9A => ("TXS", Imp, 2), 0x9D => ("STA", Abx, 5),
        0xA0 => ("LDY", Imm, 2), 0xA1 => ("LDA", Izx, 6), 0xA2 => ("LDX", Imm, 2),
        0xA4 => ("LDY", Zp0, 3), 0xA5 => ("LDA", Zp0, 3), 0xA6 => ("LDX", Zp0, 3),
        0xA8 => ("TAY", Imp, 2), 0xA9 => ("LDA", Imm, 2), 0xAA => ("TAX", Imp, 2),
        0xAC => ("LDY", Abs, 4), 0xAD => ("LDA", Abs, 4), 0xAE => ("LDX", Abs, 4),
        0xB0 => ("BCS", Rel, 2), 0xB1 => ("LDA", Izy, 5), 0xB4 => ("LDY", Zpx, 4),
        0xB5 => ("LDA", Zpx, 4), 0xB6 => ("LDX", Zpy, 4), 0xB8 => ("CLV", Imp, 2),
        0xB9 => ("LDA", Aby, 4), 0xBA => ("TSX", Imp, 2), 0xBC => ("LDY", Abx, 4),
        0xBD => ("LDA", Abx, 4), 0xBE => ("LDX", Aby, 4), 0xC0 => ("CPY", Imm, 2),
        0xC1 => ("CMP", Izx, 6), 0xC4 => ("CPY", Zp0, 3), 0xC5 => ("CMP", Zp0, 3),
        0xC6 => ("DEC", Zp0, 5), 0xC8 => ("INY", Imp, 2), 0xC9 => ("CMP", Imm, 2),
        0xCA => ("DEX", Imp, 2), 0xCC => ("CPY", Abs, 4), 0xCD => ("CMP", Abs, 4),
        0xCE => ("DEC", Abs, 6), 0xD0 => ("BNE", Rel, 2), 0xD1 => ("CMP", Izy, 5),
        0xD5 => ("CMP", Zpx, 4), 0xD6 => ("DEC", Zpx, 6), 0xD8 => ("CLD", Imp, 2),
        0xD9 => ("CMP", Aby, 4), 0xDD => ("CMP", Abx, 4), 0xDE => ("DEC", Abx, 7),
        0xE0 => ("CPX", Imm, 2), 0xE1 => ("SBC", Izx, 6), 0xE4 => ("CPX", Zp0, 3),
        0xE5 => ("SBC", Zp0, 3), 0xE6 => ("INC", Zp0, 5), 0xE8 => ("INX", Imp, 2),
        0xE9 | 0xEB => ("SBC", Imm, 2), 0xEA => ("NOP", Imp, 2), 0xEC => ("CPX", Abs, 4),
        0xED => ("SBC", Abs, 4), 0xEE => ("INC", Abs, 6), 0xF0 => ("BEQ", Rel, 2),
        0xF1 => ("SBC", Izy, 5), 0xF5 => ("SBC", Zpx, 4), 0xF6 => ("INC", Zpx, 6),
        0xF8 => ("SED", Imp, 2), 0xF9 => ("SBC", Aby, 4), 0xFD => ("SBC", Abx, 4),
        0xFE => ("INC", Abx, 7),

        // Instruções não oficiais usadas por jogos comerciais/homebrew.
        0xA3 => ("LAX", Izx, 6), 0xA7 => ("LAX", Zp0, 3), 0xAB => ("LAX", Imm, 2),
        0xAF => ("LAX", Abs, 4), 0xB3 => ("LAX", Izy, 5), 0xB7 => ("LAX", Zpy, 4),
        0xBF => ("LAX", Aby, 4),

        0x83 => ("SAX", Izx, 6), 0x87 => ("SAX", Zp0, 3),
        0x8F => ("SAX", Abs, 4), 0x97 => ("SAX", Zpy, 4),

        0xC3 => ("DCP", Izx, 8), 0xC7 => ("DCP", Zp0, 5), 0xCF => ("DCP", Abs, 6),
        0xD3 => ("DCP", Izy, 8), 0xD7 => ("DCP", Zpx, 6), 0xDB => ("DCP", Aby, 7),
        0xDF => ("DCP", Abx, 7),

        0xE3 => ("ISB", Izx, 8), 0xE7 => ("ISB", Zp0, 5), 0xEF => ("ISB", Abs, 6),
        0xF3 => ("ISB", Izy, 8), 0xF7 => ("ISB", Zpx, 6), 0xFB => ("ISB", Aby, 7),
        0xFF => ("ISB", Abx, 7),

        0x03 => ("SLO", Izx, 8), 0x07 => ("SLO", Zp0, 5), 0x0F => ("SLO", Abs, 6),
        0x13 => ("SLO", Izy, 8), 0x17 => ("SLO", Zpx, 6), 0x1B => ("SLO", Aby, 7),
        0x1F => ("SLO", Abx, 7),

        0x23 => ("RLA", Izx, 8), 0x27 => ("RLA", Zp0, 5), 0x2F => ("RLA", Abs, 6),
        0x33 => ("RLA", Izy, 8), 0x37 => ("RLA", Zpx, 6), 0x3B => ("RLA", Aby, 7),
        0x3F => ("RLA", Abx, 7),

        0x43 => ("SRE", Izx, 8), 0x47 => ("SRE", Zp0, 5), 0x4F => ("SRE", Abs, 6),
        0x53 => ("SRE", Izy, 8), 0x57 => ("SRE", Zpx, 6), 0x5B => ("SRE", Aby, 7),
        0x5F => ("SRE", Abx, 7),

        0x63 => ("RRA", Izx, 8), 0x67 => ("RRA", Zp0, 5), 0x6F => ("RRA", Abs, 6),
        0x73 => ("RRA", Izy, 8), 0x77 => ("RRA", Zpx, 6), 0x7B => ("RRA", Aby, 7),
        0x7F => ("RRA", Abx, 7),

        0x0B | 0x2B => ("ANC", Imm, 2), 0x4B => ("ALR", Imm, 2),
        0x6B => ("ARR", Imm, 2), 0xCB => ("AXS", Imm, 2),

        0x8B => ("XAA", Imm, 2), 0xBB => ("LAS", Aby, 4),
        0x93 => ("AHX", Izy, 6), 0x9F => ("AHX", Aby, 5),
        0x9C => ("SHY", Abx, 5), 0x9E => ("SHX", Aby, 5),
        0x9B => ("TAS", Aby, 5),

        // NOPs não oficiais com operandos: avançam o PC para o byte seguinte correto.
        0x80 | 0x82 | 0x89 | 0xC2 | 0xE2 => ("NOP", Imm, 2),
        0x04 | 0x44 | 0x64 => ("NOP", Zp0, 3),
        0x14 | 0x34 | 0x54 | 0x74 | 0xD4 | 0xF4 => ("NOP", Zpx, 4),
        0x0C => ("NOP", Abs, 4),
        0x1C | 0x3C | 0x5C | 0x7C | 0xDC | 0xFC => ("NOP", Abx, 4),
        _ => ("NOP", Imp, 2),
    }
}

// ============================================================================
// INICIALIZACAO DO STREAM DE AUDIO (CPAL)
// ============================================================================
fn init_audio() -> Option<(cpal::Stream, f64, Arc<Mutex<VecDeque<f32>>>)> {
    let host = cpal::default_host();
    let device = host.default_output_device()?;
    let config = device.default_output_config().ok()?;
    let sample_rate = config.sample_rate().0 as f64;
    let channels = config.channels() as usize;

    let audio_buffer = Arc::new(Mutex::new(VecDeque::<f32>::with_capacity(4096)));
    let buf_clone = Arc::clone(&audio_buffer);

    let stream_config: cpal::StreamConfig = config.into();
    let stream = device
        .build_output_stream(
            &stream_config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let mut buf = buf_clone.lock().unwrap();
                for frame in data.chunks_mut(channels) {
                    let sample = buf.pop_front().unwrap_or(0.0);
                    for ch in frame.iter_mut() {
                        *ch = sample;
                    }
                }
            },
            |err| eprintln!("Erro no stream de audio: {}", err),
            None,
        )
        .ok()?;

    stream.play().ok()?;
    Some((stream, sample_rate, audio_buffer))
}

// ============================================================================
// SISTEMA DE ESTATÍSTICAS UNIVERSAL (TEMPO DE JOGO E SESSÕES)
// ============================================================================
#[derive(Clone, Default)]
pub struct GameStats {
    pub play_time_seconds: u64,
    pub play_count: u32,
    pub is_favorite: bool,
    pub last_played_epoch: u64,
}

fn get_stats_file(rom_name: &str) -> PathBuf {
    let dir = PathBuf::from(".stats");
    let _ = fs::create_dir_all(&dir);
    let clean = sanitize_rom_name_for_scraping(rom_name).replace(' ', "_");
    dir.join(format!("{}.dat", clean))
}

fn load_game_stats(rom_name: &str) -> GameStats {
    let path = get_stats_file(rom_name);
    if let Ok(bytes) = fs::read(&path) {
        if bytes.len() >= 12 {
            let play_time_seconds = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
            let play_count = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
            return GameStats {
                play_time_seconds,
                play_count,
                is_favorite: bytes.get(12).copied().unwrap_or(0) != 0,
                last_played_epoch: if bytes.len() >= 21 { u64::from_le_bytes(bytes[13..21].try_into().unwrap()) } else { 0 },
            };
        }
    }
    GameStats::default()
}

fn save_game_stats(rom_name: &str, stats: &GameStats) {
    let path = get_stats_file(rom_name);
    let mut bytes = Vec::with_capacity(21);
    bytes.extend_from_slice(&stats.play_time_seconds.to_le_bytes());
    bytes.extend_from_slice(&stats.play_count.to_le_bytes());
    bytes.push(stats.is_favorite as u8);
    bytes.extend_from_slice(&stats.last_played_epoch.to_le_bytes());
    let _ = fs::write(path, bytes);
}

#[derive(Clone, Copy, PartialEq)]
pub enum HubCategory { All, Favorites, Recents }

impl HubCategory {
    fn next(self) -> Self {
        match self { Self::All => Self::Favorites, Self::Favorites => Self::Recents, Self::Recents => Self::All }
    }
    fn label(self) -> &'static str {
        match self { Self::All => "TODOS", Self::Favorites => "* FAVORITOS", Self::Recents => "RECENTES" }
    }
}

fn rebuild_filtered_list(roms: &[RomEntry], category: HubCategory, search_query: &str) -> Vec<usize> {
    let query = search_query.trim().to_uppercase();
    let mut indices: Vec<usize> = roms.iter().enumerate().filter(|(_, rom)| {
        (query.is_empty() || rom.clean_upper.contains(&query)) && match category {
            HubCategory::All => true,
            HubCategory::Favorites => rom.stats.is_favorite,
            HubCategory::Recents => rom.stats.play_count > 0,
        }
    }).map(|(idx, _)| idx).collect();
    if category == HubCategory::Recents {
        indices.sort_by(|&a, &b| roms[b].stats.last_played_epoch.cmp(&roms[a].stats.last_played_epoch));
    }
    indices
}

// ============================================================================
// SISTEMA DE HUB ARCADE MODERNO (CARROSSEL HORIZONTAL ESTILO REDMAGIC / STEAM DECK)
// ============================================================================
pub struct RomEntry {
    pub name: String,
    pub clean_upper: String,
    pub first_char: char,
    pub path: String,
    pub mapper: u8,
    pub prg_kb: usize,
    pub chr_kb: usize,
    pub has_battery: bool,
    pub preview: Vec<u32>, // 90x130 pixels (Formato vertical de alta definição Box Art)
    pub theme_color: u32,
    pub stats: GameStats,
    pub cover_checked: bool,
}

fn generate_fast_fallback_card(name: &str, header: &[u8]) -> (u8, usize, usize, bool, Vec<u32>, u32) {
    let card_w = 90;
    let card_h = 130;
    let mut mapper = 0;
    let mut prg_kb = 0;
    let mut chr_kb = 0;
    let mut has_battery = false;

    if header.len() >= 16 && &header[0..4] == b"NES\x1A" {
        prg_kb = (header[4] as usize) * 16;
        chr_kb = (header[5] as usize) * 8;
        let flags6 = header[6];
        let flags7 = header[7];
        mapper = (flags7 & 0xF0) | (flags6 >> 4);
        has_battery = (flags6 & 0b0010) != 0;
    }

    let theme_color = match mapper {
        0 => 0xE52521, // Vermelho NES
        1 => 0x2A62C4, // Azul MMC1
        2 => 0x2E7D32, // Verde UxROM
        3 => 0xF57C00, // Laranja CNROM
        4 => 0x8E24AA, // Roxo MMC3
        _ => 0x00E5FF, // Ciano
    };

    let mut thumb = vec![0x10141E; card_w * card_h];
    for y in 0..card_h {
        let t = y as f32 / card_h as f32;
        let r = (20.0 * (1.0 - t) + 12.0 * t) as u32;
        let g = (26.0 * (1.0 - t) + 16.0 * t) as u32;
        let b = (40.0 * (1.0 - t) + 24.0 * t) as u32;
        let c = (r << 16) | (g << 8) | b;
        for x in 0..card_w {
            thumb[y * card_w + x] = c;
        }
    }

    // Moldura decorativa do poster
    draw_rect(&mut thumb, card_w, card_h, 4, 4, card_w - 8, card_h - 8, 0x1A2234);
    draw_rect(&mut thumb, card_w, card_h, 4, 4, card_w - 8, 4, theme_color);

    // Cartucho estilizado no centro
    draw_rect(&mut thumb, card_w, card_h, 18, 30, 54, 48, if has_battery { 0xC5A059 } else { 0x37474F });
    draw_rect(&mut thumb, card_w, card_h, 24, 38, 42, 28, 0x18202F);
    draw_rect(&mut thumb, card_w, card_h, 24, 38, 42, 4, theme_color);
    draw_text(&mut thumb, card_w, card_h, "ORIGINAL", 25, 46, 0xFFFFFF);
    draw_text(&mut thumb, card_w, card_h, "NES ROM", 26, 56, 0x00E5FF);

    // Título estilizado no rodapé da capa
    let label = sanitize_rom_name_for_scraping(name);
    let short_label = if label.chars().count() > 12 {
        format!("{}..", label.chars().take(10).collect::<String>())
    } else {
        label
    };
    let lx = (card_w.saturating_sub(short_label.len() * 6)) / 2;
    draw_text(&mut thumb, card_w, card_h, &short_label, lx, 96, 0xFFFFFF);

    (mapper, prg_kb, chr_kb, has_battery, thumb, theme_color)
}

fn scan_roms_in_directory(dir: &str) -> Vec<RomEntry> {
    let mut roms = Vec::new();
    let cover_cache_dir = get_cover_cache_dir();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension() {
                    if ext.to_string_lossy().eq_ignore_ascii_case("nes") {
                        let name = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
                        let path_str = path.to_string_lossy().to_string();
                        let mut header = [0u8; 16];
                        if let Ok(mut file) = fs::File::open(&path) {
                            let _ = file.read(&mut header);
                        }
                        let (mapper, prg_kb, chr_kb, has_battery, mut preview, theme_color) =
                            generate_fast_fallback_card(&name, &header);

                        let clean_title = sanitize_rom_name_for_scraping(&name);
                        let cache_file = cover_cache_dir
                            .join(format!("{}.raw", clean_title.replace(' ', "_")));
                        let mut cover_checked = false;
                        if let Ok(bytes) = fs::read(&cache_file) {
                            if bytes.len() == 90 * 130 * 4 {
                                for (i, chunk) in bytes.chunks_exact(4).enumerate() {
                                    preview[i] = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                                }
                                cover_checked = true;
                            }
                        }

                        let clean_upper = clean_title.to_uppercase();
                        let first_char = clean_upper.chars().next()
                            .map(|ch| if ch.is_ascii_alphabetic() { ch } else { '#' })
                            .unwrap_or('#');
                        let stats = load_game_stats(&name);
                        roms.push(RomEntry {
                            name,
                            clean_upper,
                            first_char,
                            path: path_str,
                            mapper,
                            prg_kb,
                            chr_kb,
                            has_battery,
                            preview,
                            theme_color,
                            stats,
                            cover_checked,
                        });
                    }
                }
            }
        }
    }
    roms.sort_by(|a, b| a.clean_upper.cmp(&b.clean_upper));
    roms
}

pub struct AsyncCoverLoader {
    req_tx: std::sync::mpsc::Sender<(String, String)>,
    res_rx: std::sync::mpsc::Receiver<(String, Vec<u32>)>,
}

impl AsyncCoverLoader {
    pub fn new() -> Self {
        let (req_tx, req_rx) = std::sync::mpsc::channel::<(String, String)>();
        let (res_tx, res_rx) = std::sync::mpsc::channel::<(String, Vec<u32>)>();
        std::thread::spawn(move || {
            while let Ok((rom_path, rom_name)) = req_rx.recv() {
                if let Some(pixels) = download_or_load_boxart(&rom_name, 90, 130) {
                    let _ = res_tx.send((rom_path, pixels));
                }
            }
        });
        Self { req_tx, res_rx }
    }

    pub fn poll_and_request(&self, roms: &mut [RomEntry], visible_indices: &[usize]) {
        while let Ok((path, pixels)) = self.res_rx.try_recv() {
            if let Some(rom) = roms.iter_mut().find(|rom| rom.path == path) {
                rom.preview = pixels;
            }
        }

        for &idx in visible_indices {
            if idx < roms.len() && !roms[idx].cover_checked {
                roms[idx].cover_checked = true;
                let _ = self.req_tx.send((roms[idx].path.clone(), roms[idx].name.clone()));
            }
        }
    }
}

// Fonte bitmap simples 5x7 para escrever no HUB
const FONT_5X7: [[u8; 5]; 96] = [
    [0x00, 0x00, 0x00, 0x00, 0x00], [0x00, 0x00, 0x5F, 0x00, 0x00], [0x00, 0x07, 0x00, 0x07, 0x00],
    [0x14, 0x7F, 0x14, 0x7F, 0x14], [0x24, 0x2A, 0x7F, 0x2A, 0x12], [0x23, 0x13, 0x08, 0x64, 0x62],
    [0x36, 0x49, 0x55, 0x22, 0x50], [0x00, 0x05, 0x03, 0x00, 0x00], [0x00, 0x1C, 0x22, 0x41, 0x00],
    [0x00, 0x41, 0x22, 0x1C, 0x00], [0x14, 0x08, 0x3E, 0x08, 0x14], [0x08, 0x08, 0x3E, 0x08, 0x08],
    [0x00, 0x50, 0x30, 0x00, 0x00], [0x08, 0x08, 0x08, 0x08, 0x08], [0x00, 0x60, 0x60, 0x00, 0x00],
    [0x20, 0x10, 0x08, 0x04, 0x02], [0x3E, 0x51, 0x49, 0x45, 0x3E], [0x00, 0x42, 0x7F, 0x40, 0x00],
    [0x42, 0x61, 0x51, 0x49, 0x46], [0x21, 0x41, 0x45, 0x4B, 0x31], [0x18, 0x14, 0x12, 0x7F, 0x10],
    [0x27, 0x45, 0x45, 0x45, 0x39], [0x3C, 0x4A, 0x49, 0x49, 0x30], [0x01, 0x71, 0x09, 0x05, 0x03],
    [0x36, 0x49, 0x49, 0x49, 0x36], [0x06, 0x49, 0x49, 0x29, 0x1E], [0x00, 0x36, 0x36, 0x00, 0x00],
    [0x00, 0x56, 0x36, 0x00, 0x00], [0x08, 0x14, 0x22, 0x41, 0x00], [0x14, 0x14, 0x14, 0x14, 0x14],
    [0x00, 0x41, 0x22, 0x14, 0x08], [0x02, 0x01, 0x51, 0x09, 0x06], [0x32, 0x49, 0x79, 0x41, 0x3E],
    [0x7E, 0x11, 0x11, 0x11, 0x7E], [0x7F, 0x49, 0x49, 0x49, 0x36], [0x3E, 0x41, 0x41, 0x41, 0x22],
    [0x7F, 0x41, 0x41, 0x22, 0x1C], [0x7F, 0x49, 0x49, 0x49, 0x41], [0x7F, 0x09, 0x09, 0x09, 0x01],
    [0x3E, 0x41, 0x49, 0x49, 0x7A], [0x7F, 0x08, 0x08, 0x08, 0x7F], [0x00, 0x41, 0x7F, 0x41, 0x00],
    [0x20, 0x40, 0x41, 0x3F, 0x01], [0x7F, 0x08, 0x14, 0x22, 0x41], [0x7F, 0x40, 0x40, 0x40, 0x40],
    [0x7F, 0x02, 0x0C, 0x02, 0x7F], [0x7F, 0x04, 0x08, 0x10, 0x7F], [0x3E, 0x41, 0x41, 0x41, 0x3E],
    [0x7F, 0x09, 0x09, 0x09, 0x06], [0x3E, 0x41, 0x51, 0x21, 0x5E], [0x7F, 0x09, 0x19, 0x29, 0x46],
    [0x46, 0x49, 0x49, 0x49, 0x31], [0x01, 0x01, 0x7F, 0x01, 0x01], [0x3F, 0x40, 0x40, 0x40, 0x3F],
    [0x1F, 0x20, 0x40, 0x20, 0x1F], [0x7F, 0x20, 0x18, 0x20, 0x7F], [0x63, 0x14, 0x08, 0x14, 0x63],
    [0x07, 0x08, 0x70, 0x08, 0x07], [0x61, 0x51, 0x49, 0x45, 0x43], [0x00, 0x7F, 0x41, 0x41, 0x00],
    [0x02, 0x04, 0x08, 0x10, 0x20], [0x00, 0x41, 0x41, 0x7F, 0x00], [0x04, 0x02, 0x01, 0x02, 0x04],
    [0x40, 0x40, 0x40, 0x40, 0x40], [0x00, 0x01, 0x02, 0x04, 0x00], [0x20, 0x54, 0x54, 0x54, 0x78],
    [0x7F, 0x48, 0x44, 0x44, 0x38], [0x38, 0x44, 0x44, 0x44, 0x20], [0x38, 0x44, 0x44, 0x48, 0x7F],
    [0x38, 0x54, 0x54, 0x54, 0x18], [0x08, 0x7E, 0x09, 0x01, 0x02], [0x0C, 0x52, 0x52, 0x52, 0x3E],
    [0x7F, 0x08, 0x04, 0x04, 0x78], [0x00, 0x44, 0x7D, 0x40, 0x00], [0x20, 0x40, 0x44, 0x3D, 0x00],
    [0x7F, 0x10, 0x28, 0x44, 0x00], [0x00, 0x41, 0x7F, 0x40, 0x00], [0x7C, 0x04, 0x18, 0x04, 0x78],
    [0x7C, 0x08, 0x04, 0x04, 0x78], [0x38, 0x44, 0x44, 0x44, 0x38], [0x7C, 0x14, 0x14, 0x14, 0x08],
    [0x08, 0x14, 0x14, 0x18, 0x7C], [0x7C, 0x08, 0x04, 0x04, 0x08], [0x48, 0x54, 0x54, 0x54, 0x20],
    [0x04, 0x3F, 0x44, 0x40, 0x20], [0x3C, 0x40, 0x40, 0x20, 0x7C], [0x1C, 0x20, 0x40, 0x20, 0x1C],
    [0x3C, 0x40, 0x30, 0x40, 0x3C], [0x44, 0x28, 0x10, 0x28, 0x44], [0x0C, 0x50, 0x50, 0x50, 0x3C],
    [0x44, 0x64, 0x54, 0x4C, 0x44], [0x00, 0x08, 0x36, 0x41, 0x00], [0x00, 0x00, 0x7F, 0x00, 0x00],
    [0x00, 0x41, 0x36, 0x08, 0x00], [0x08, 0x04, 0x08, 0x10, 0x08], [0x7F, 0x7F, 0x7F, 0x7F, 0x7F],
];

fn draw_text(buffer: &mut [u32], width: usize, height: usize, text: &str, x: usize, y: usize, color: u32) {
    let mut cur_x = x;
    for ch in text.chars() {
        let code = ch as usize;
        if (32..127).contains(&code) {
            let glyph = &FONT_5X7[code - 32];
            for (col_idx, &col) in glyph.iter().enumerate() {
                for row_idx in 0..7 {
                    if (col & (1 << row_idx)) != 0 {
                        let px = cur_x + col_idx;
                        let py = y + row_idx;
                        if px < width && py < height {
                            buffer[py * width + px] = color;
                        }
                    }
                }
            }
        }
        cur_x += 6;
    }
}

fn draw_rect(buffer: &mut [u32], width: usize, height: usize, x: usize, y: usize, w: usize, h: usize, color: u32) {
    for row in y..(y + h).min(height) {
        for col in x..(x + w).min(width) {
            buffer[row * width + col] = color;
        }
    }
}

// ============================================================================
// DESENHO DE BOTÕES DE GAMEPAD ESTILO XBOX (A, B, X, Y, LB, RB, D-PAD)
// ============================================================================
fn draw_button_badge(
    buffer: &mut [u32],
    canvas_w: usize,
    canvas_h: usize,
    label: &str,
    x: usize,
    y: usize,
    bg_color: u32,
    text_color: u32,
) {
    let pad_w = (label.len() * 6 + 6).max(12);
    let pad_h = 11;

    // Fundo do botão com cantos chanfrados (aparência arredondada de badge)
    for row in 0..pad_h {
        let py = y + row;
        if py >= canvas_h { continue; }
        for col in 0..pad_w {
            let px = x + col;
            if px >= canvas_w { continue; }

            // Recorta os 4 cantos para dar formato de pílula/círculo
            let is_corner = (row == 0 || row == pad_h - 1) && (col == 0 || col == pad_w - 1);
            if !is_corner {
                buffer[py * canvas_w + px] = bg_color;
            }
        }
    }

    // Texto centralizado dentro do botão
    let text_x = x + (pad_w.saturating_sub(label.len() * 6)) / 2;
    draw_text(buffer, canvas_w, canvas_h, label, text_x, y + 2, text_color);
}

fn draw_rounded_card(
    buffer: &mut [u32],
    card_x: i32,
    card_y: i32,
    card_w: usize,
    card_h: usize,
    preview: &[u32],
    is_focused: bool,
    theme_color: u32,
) {
    let border_color = if is_focused { 0x00E5FF } else { 0x242D3D };

    for y in 0..card_h {
        let dy = card_y + y as i32;
        // Limite vertical corrigido para CANVAS_HEIGHT (288)
        if dy < 0 || dy >= CANVAS_HEIGHT as i32 { continue; }

        for x in 0..card_w {
            let dx = card_x + x as i32;
            // Limite horizontal corrigido para CANVAS_WIDTH (512)
            if dx < 0 || dx >= CANVAS_WIDTH as i32 { continue; }

            // Borda e destaque de foco
            let is_border = x == 0 || x == card_w - 1 || y == 0 || y == card_h - 1;
            let is_inner = is_focused && (x == 1 || x == card_w - 2 || y == 1 || y == card_h - 2);

            let color = if is_border {
                border_color
            } else if is_inner {
                theme_color
            } else {
                let sample_x = (x * 90) / card_w;
                let sample_y = (y * 130) / card_h;
                let pixel = preview[sample_y * 90 + sample_x];
                if is_focused {
                    pixel
                } else {
                    // Escurece suavemente os cards laterais
                    let r = (((pixel >> 16) & 0xFF) * 4) / 10;
                    let g = (((pixel >> 8) & 0xFF) * 4) / 10;
                    let b = ((pixel & 0xFF) * 4) / 10;
                    (r << 16) | (g << 8) | b
                }
            };

            // Índice de memória corrigido para largura 512 (CANVAS_WIDTH)
            buffer[(dy as usize) * CANVAS_WIDTH + (dx as usize)] = color;
        }
    }
}

// ============================================================================
// RENDERIZAÇÃO DO HUB ARCADE EM ALTA RESOLUÇÃO (512x288)
// ============================================================================
const VIRTUAL_KB_CHARS: &[&str] = &[
    "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M",
    "N", "O", "P", "Q", "R", "S", "T", "U", "V", "W", "X", "Y", "Z",
    "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "ESPACO", "APAGAR", "LIMPAR", "OK",
];

fn render_arcade_hub_frame(
    buffer: &mut [u32],
    roms: &[RomEntry],
    filtered_indices: &[usize],
    selected_pos: usize, // Posição dentro de filtered_indices
    category: HubCategory,
    gamepad_connected: bool,
    in_settings_menu: bool,
    in_search_mode: bool,
    search_query: &str,
    vkb_cursor: usize,
) {
    let active_theme = if !filtered_indices.is_empty() {
        roms[filtered_indices[selected_pos]].theme_color
    } else {
        0x00E5FF
    };

    let tr = ((active_theme >> 16) & 0xFF) as f32;
    let tg = ((active_theme >> 8) & 0xFF) as f32;
    let tb = (active_theme & 0xFF) as f32;

    for y in 0..CANVAS_HEIGHT {
        let factor = (y as f32 / CANVAS_HEIGHT as f32) * 0.7;
        let r = ((tr * 0.15) * (1.0 - factor) + 12.0 * factor) as u32;
        let g = ((tg * 0.15) * (1.0 - factor) + 16.0 * factor) as u32;
        let b = ((tb * 0.18) * (1.0 - factor) + 26.0 * factor) as u32;
        let bg_col = (r << 16) | (g << 8) | b;
        for x in 0..CANVAS_WIDTH {
            buffer[y * CANVAS_WIDTH + x] = bg_col;
        }
    }

    for y in (44..CANVAS_HEIGHT).step_by(16) {
        for x in (0..CANVAS_WIDTH).step_by(16) {
            buffer[y * CANVAS_WIDTH + x] = 0x222C3E;
        }
    }

    // Barra Superior
    draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, 0, 0, CANVAS_WIDTH, 24, 0x0C101A);
    draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, 10, 5, 5, 5, 0x00E5FF);
    draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, 17, 5, 5, 5, 0x00E5FF);
    draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, 10, 12, 5, 5, 0x00E5FF);
    draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, 17, 12, 5, 5, 0x00E5FF);
    draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "ARCADE LIBRARY", 28, 8, 0xFFFFFF);

    // Contador de Jogos e Filtro de Busca ativo no topo
    let count_str = if search_query.is_empty() {
        format!("{}: {}/{}", category.label(), if filtered_indices.is_empty() { 0 } else { selected_pos + 1 }, filtered_indices.len())
    } else {
        format!("{} '{}': {}/{}", category.label(), search_query, if filtered_indices.is_empty() { 0 } else { selected_pos + 1 }, filtered_indices.len())
    };
    draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, &count_str, 145, 8, 0x00E5FF);

    let pad_color = if gamepad_connected { 0x81C784 } else { 0x455A64 };
    draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "XBOX PAD", 325, 8, pad_color);

    let total_secs = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_secs();
    let time_str = format!("{:02}:{:02}", (total_secs / 3600) % 24, (total_secs / 60) % 60);
    draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, &time_str, 390, 8, 0xCFD8DC);

    draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, 430, 4, 75, 16, if in_settings_menu { 0x283852 } else { 0x182030 });
    if gamepad_connected {
        draw_button_badge(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "START", 434, 6, 0x1565C0, 0xFFFFFF);
        draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "OPCOES", 474, 8, 0xFFFFFF);
    } else {
        draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "F3:MENU", 445, 8, 0x90A4AE);
    }

    // BARRA ALFABÉTICA DE SALTO RÁPIDO (# A B C ... Z)
    draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, 0, 24, CANVAS_WIDTH, 16, 0x111726);
    draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, 0, 40, CANVAS_WIDTH, 1, 0x1E273A);

    let current_letter = if !filtered_indices.is_empty() {
        roms[filtered_indices[selected_pos]].first_char
    } else {
        ' '
    };

    let alphabet = "#ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let start_alpha_x = 34;
    draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "LB<", 8, 28, 0x607D8B);
    for (i, ch) in alphabet.chars().enumerate() {
        let ax = start_alpha_x + i * 16;
        let is_cur = ch == current_letter;
        if is_cur {
            draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, ax - 3, 25, 11, 13, 0x00E5FF);
        }
        let s = ch.to_string();
        draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, &s, ax, 28, if is_cur { 0x000000 } else { 0x78909C });
    }
    draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, ">RB", 482, 28, 0x607D8B);

    if filtered_indices.is_empty() {
        draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, 96, 95, 320, 100, 0x151B26);
        draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, 96, 95, 320, 2, 0x00E5FF);
        if roms.is_empty() {
            draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "PASTA 'roms' PRONTA!", 195, 115, 0x00E5FF);
            draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "Coloque seus jogos .nes na pasta 'roms'", 135, 140, 0xFFFFFF);
            draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "ou pressione F1 / (Y) para outra pasta", 138, 160, 0x90A4AE);
        } else {
            draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "NENHUM JOGO COM ESSE NOME", 178, 125, 0xFF5252);
            draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "Aperte X / F2 para limpar a busca", 155, 150, 0xCFD8DC);
        }
    } else {
        // CARROSSEL DE CARDS
        let center_card_w = 100;
        let center_card_h = 140;
        let center_card_x = (CANVAS_WIDTH as i32 - center_card_w as i32) / 2;
        let center_card_y = 46;

        let side_card_w = 76;
        let side_card_h = 108;
        let side_card_y = 62;

        let total = filtered_indices.len() as i32;
        let sel = selected_pos as i32;

        for offset in [-3, -2, -1, 1, 2, 3] {
            let pos = sel + offset;
            if pos >= 0 && pos < total {
                let rom_idx = filtered_indices[pos as usize];
                let spacing = 96;
                let card_x = if offset < 0 {
                    center_card_x + offset * spacing - 6
                } else {
                    center_card_x + center_card_w as i32 + (offset - 1) * spacing + 18
                };
                draw_rounded_card(
                    buffer,
                    card_x,
                    side_card_y,
                    side_card_w,
                    side_card_h,
                    &roms[rom_idx].preview,
                    false,
                    roms[rom_idx].theme_color,
                );
            }
        }

        let sel_rom = &roms[filtered_indices[selected_pos]];
        draw_rounded_card(
            buffer,
            center_card_x,
            center_card_y,
            center_card_w,
            center_card_h,
            &sel_rom.preview,
            true,
            active_theme,
        );
        if sel_rom.stats.is_favorite {
            draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, center_card_x as usize + 58, center_card_y as usize + 6, 36, 12, 0xFFD54F);
            draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "*FAV", center_card_x as usize + 64, center_card_y as usize + 9, 0x000000);
        }

        let display_title = sanitize_rom_name_for_scraping(&sel_rom.name);
        let title_x = (CANVAS_WIDTH / 2).saturating_sub((display_title.len() * 6) / 2);
        draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, &display_title, title_x, 195, 0xFFFFFF);

        let info_str = format!(
            "MAPPER {} | {}KB PRG | {}KB CHR | {}",
            sel_rom.mapper,
            sel_rom.prg_kb,
            sel_rom.chr_kb,
            if sel_rom.has_battery { "BATERIA (SRAM)" } else { "STANDARD" }
        );
        let info_x = (CANVAS_WIDTH / 2).saturating_sub((info_str.len() * 6) / 2);
        draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, &info_str, info_x, 212, 0x00E5FF);

        let btn_w = 160;
        let btn_x = (CANVAS_WIDTH - btn_w) / 2;
        draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, btn_x, 232, btn_w, 22, 0x1A2538);
        draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, btn_x, 232, btn_w, 1, 0x00E5FF);
        draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, btn_x, 253, btn_w, 1, 0x00E5FF);

        if gamepad_connected {
            draw_button_badge(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "A", btn_x + 30, 237, 0x2E7D32, 0xFFFFFF);
            draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "JOGAR AGORA", btn_x + 48, 239, 0xFFFFFF);
        } else {
            draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "> PRESSIONE ENTER <", btn_x + 23, 239, 0xFFFFFF);
        }
    }

    // MODAL DE BUSCA (TECLADO FÍSICO + TECLADO VIRTUAL PARA CONTROLE)
    if in_search_mode {
        let mw = 390;
        let mh = 135;
        let mx = (CANVAS_WIDTH - mw) / 2;
        let my = 80;

        draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, mx - 1, my - 1, mw + 2, mh + 2, 0x00E5FF);
        draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, mx, my, mw, mh, 0x0B101B);
        draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, mx, my, mw, 28, 0x141E30);

        let prompt = format!("BUSCAR JOGO: {}_", search_query);
        draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, &prompt, mx + 14, my + 10, 0x00E5FF);

        // Grade do Teclado Virtual (10 colunas x 4 linhas)
        let cols = 10;
        for (i, &key_label) in VIRTUAL_KB_CHARS.iter().enumerate() {
            let r = i / cols;
            let c = i % cols;
            let kx = mx + 12 + c * 37;
            let ky = my + 38 + r * 22;
            let kw = 34;
            let kh = 18;
            let is_sel = i == vkb_cursor;

            draw_rect(
                buffer,
                CANVAS_WIDTH,
                CANVAS_HEIGHT,
                kx,
                ky,
                kw,
                kh,
                if is_sel { 0x00E5FF } else { 0x18243A },
            );
            let short_k = match key_label {
                "ESPACO" => "SPC",
                "APAGAR" => "DEL",
                "LIMPAR" => "CLR",
                other => other,
            };
            let tx = kx + (kw.saturating_sub(short_k.len() * 6)) / 2;
            draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, short_k, tx, ky + 5, if is_sel { 0x000000 } else { 0xFFFFFF });
        }
    }

    // Rodapé com os novos atalhos de Salto A-Z e Busca
    draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, 0, 268, CANVAS_WIDTH, 20, 0x0A0D14);
    draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, 0, 268, CANVAS_WIDTH, 1, 0x1A2234);

    if gamepad_connected {
        let mut cx = 10;
        draw_button_badge(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "LB/RB", cx, 272, 0x37474F, 0xFFFFFF);
        cx += 38;
        draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "LETRA A-Z", cx, 274, 0x90A4AE);
        cx += 64;

        draw_button_badge(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "X", cx, 272, 0x1565C0, 0xFFFFFF);
        cx += 16;
        draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "BUSCAR", cx, 274, 0x90A4AE);
        cx += 48;

        draw_button_badge(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "A", cx, 272, 0x2E7D32, 0xFFFFFF);
        cx += 16;
        draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "JOGAR", cx, 274, 0x90A4AE);
        cx += 42;

        draw_button_badge(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "Y", cx, 272, 0xF57F17, 0xFFFFFF);
        cx += 16;
        draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "PASTA", cx, 274, 0x90A4AE);
        cx += 42;

        draw_button_badge(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "START", cx, 272, 0x1565C0, 0xFFFFFF);
        cx += 38;
        draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "MENU", cx, 274, 0x90A4AE);
    } else {
        draw_text(
            buffer,
            CANVAS_WIDTH,
            CANVAS_HEIGHT,
            "PGUP/PGDN: LETRA A-Z   F2: BUSCAR   ENTER: JOGAR   F1: PASTA   F3: MENU",
            28,
            274,
            0x78909C,
        );
    }
}

fn roms_char(roms: &[RomEntry], filtered: &[usize], pos: usize) -> char {
    filtered.get(pos)
        .and_then(|&idx| roms.get(idx))
        .map(|rom| rom.first_char)
        .unwrap_or('#')
}

// ============================================================================
// NOVO MENU DE OPÇÕES PROFISSIONAL COM ABAS (VÍDEO / ÁUDIO / SISTEMA)
// ============================================================================
fn render_advanced_settings_menu(
    buffer: &mut [u32],
    settings: &EmulatorSettings,
    active_tab: usize,       // 0: VIDEO, 1: AUDIO, 2: SISTEMA, 3: CONTROLES
    row_cursor: usize,       // Linha selecionada dentro da aba
    gamepad_connected: bool,
) {
    // 1. Escurecimento translúcido do fundo
    for px in buffer.iter_mut() {
        let p = *px;
        let r = (((p >> 16) & 0xFF) * 2) / 10;
        let g = (((p >> 8) & 0xFF) * 2) / 10;
        let b = ((p & 0xFF) * 2) / 10;
        *px = (r << 16) | (g << 8) | b;
    }

    let mw = 380;
    let mh = 220;
    let mx = (CANVAS_WIDTH - mw) / 2;
    let my = (CANVAS_HEIGHT - mh) / 2;

    // Moldura principal com borda ciano neon
    draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, mx - 1, my - 1, mw + 2, mh + 2, 0x00E5FF);
    draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, mx, my, mw, mh, 0x0B0F19);

    // Barra de Cabeçalho / Abas
    draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, mx, my, mw, 26, 0x131A29);
    draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, mx, my + 26, mw, 1, 0x1E2B42);

    let tabs = ["1.VIDEO", "2.AUDIO", "3.SISTEMA", "4.CONTROLE"];
    let tab_w = mw / tabs.len();

    for (t_idx, &t_name) in tabs.iter().enumerate() {
        let tx = mx + t_idx * tab_w;
        let is_tab_sel = t_idx == active_tab;

        if is_tab_sel {
            draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, tx, my + 1, tab_w, 25, 0x1D2A42);
            draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, tx, my + 24, tab_w, 2, 0x00E5FF);
        }
        let tc = if is_tab_sel { 0x00E5FF } else { 0x607D8B };
        let text_x = tx + (tab_w.saturating_sub(t_name.len() * 6)) / 2;
        draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, t_name, text_x, my + 9, tc);
    }

    // Itens de cada Aba
    let items: Vec<(String, String)> = match active_tab {
        0 => vec![
            ("MODO DE TELA".into(), match settings.display_mode {
                DisplayMode::Windowed => "< JANELA (1024x576) >".into(),
                DisplayMode::Borderless => "< SEM BORDAS >".into(),
                DisplayMode::Fullscreen => "< TELA CHEIA >".into(),
            }),
            ("FORMATO DE TELA".into(), settings.aspect_label().into()),
            ("FILTRO SCANLINES".into(), if settings.crt_scanlines { "< ATIVADO >".into() } else { "< DESATIVADO >".into() }),
            ("LIMITE 8 SPRITES".into(), if settings.sprite_limit { "< ORIGINAL (FLICKER) >".into() } else { "< SEM LIMITE >".into() }),
        ],
        1 => vec![
            ("VOLUME GERAL".into(), format!("< {}% >", (settings.volume * 100.0).round() as usize)),
            ("MUTAR AUDIO (MUTE)".into(), if settings.mute_all { "< ATIVADO >".into() } else { "< DESATIVADO >".into() }),
        ],
        2 => vec![
            ("SLOT SAVE ATIVO".into(), format!("< SLOT {} >", settings.active_save_slot)),
            ("VELOCIDADE TURBO".into(), format!("< {}X >", settings.turbo_multiplier)),
            ("REGIAO DO CONSOLE".into(), "< NTSC (60 HZ) >".into()),
        ],
        3 => vec![
            ("DISPOSITIVO".into(), if gamepad_connected { "XBOX GAMEPAD (CONECTADO)".into() } else { "TECLADO PADRAO (USB)".into() }),
            ("BOTAO A / B".into(), if gamepad_connected { "A = A / B = B".into() } else { "Z = A / X = B".into() }),
            ("TURBO / MENU".into(), if gamepad_connected { "RB = TURBO / START = MENU".into() } else { "TAB = TURBO / ESC = MENU".into() }),
        ],
        _ => vec![],
    };

    let start_y = my + 44;
    for (i, (label, val)) in items.iter().enumerate() {
        let iy = start_y + i * 30;
        let is_sel = i == row_cursor;

        if is_sel {
            draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, mx + 12, iy - 4, mw - 24, 22, 0x16243D);
            draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, mx + 12, iy - 4, 3, 22, 0x00E5FF);
        }

        let l_color = if is_sel { 0xFFFFFF } else { 0x90A4AE };
        let v_color = if is_sel { 0x00E5FF } else { 0xCFD8DC };

        draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, label, mx + 24, iy + 3, l_color);
        let val_x = mx + mw - 24 - (val.len() * 6);
        draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, val, val_x, iy + 3, v_color);
    }

    // Rodapé de instruções do menu
    let foot_y = my + mh - 20;
    draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, mx, foot_y - 4, mw, 1, 0x1A253A);

    if gamepad_connected {
        draw_button_badge(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "LB/RB", mx + 14, foot_y, 0x37474F, 0xFFFFFF);
        draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "ABAS", mx + 50, foot_y + 2, 0x90A4AE);

        draw_button_badge(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "< >", mx + 88, foot_y, 0x37474F, 0xFFFFFF);
        draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "AJUSTAR", mx + 116, foot_y + 2, 0x90A4AE);

        draw_button_badge(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "B", mx + 180, foot_y, 0xC62828, 0xFFFFFF);
        draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "FECHAR", mx + 196, foot_y + 2, 0x90A4AE);
    } else {
        draw_text(
            buffer,
            CANVAS_WIDTH,
            CANVAS_HEIGHT,
            "1-4: ABAS   CIMA/BAIXO: ITENS   < >: ALTERAR   ESC: FECHAR",
            mx + 18,
            foot_y + 2,
            0x78909C,
        );
    }
}

// ============================================================================
// MODAL DE DETALHES DO JOGO (GAME DETAIL CARD)
// ============================================================================
fn render_game_detail_card(
    buffer: &mut [u32],
    rom: &RomEntry,
    gamepad_connected: bool,
    card_cursor: usize, // 0 = INICIAR JOGO, 1 = ZERAR STATS, 2 = VOLTAR
) {
    // Escurece o fundo
    for px in buffer.iter_mut() {
        let p = *px;
        let r = (((p >> 16) & 0xFF) * 3) / 10;
        let g = (((p >> 8) & 0xFF) * 3) / 10;
        let b = ((p & 0xFF) * 3) / 10;
        *px = (r << 16) | (g << 8) | b;
    }

    let mw = 360;
    let mh = 195;
    let mx = (CANVAS_WIDTH - mw) / 2;
    let my = (CANVAS_HEIGHT - mh) / 2;

    draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, mx - 1, my - 1, mw + 2, mh + 2, 0x00E5FF);
    draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, mx, my, mw, mh, 0x0C121D);
    draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, mx, my, mw, 24, 0x162238);

    // Título do Jogo
    let title = sanitize_rom_name_for_scraping(&rom.name);
    let title_short = if title.len() > 28 { format!("{}...", &title[..25]) } else { title };
    draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, &title_short, mx + 14, my + 8, 0xFFFFFF);

    // Capa do Jogo à esquerda
    let thumb_w = 70;
    let thumb_h = 100;
    let thumb_x = mx + 16;
    let thumb_y = my + 38;

    for y in 0..thumb_h {
        for x in 0..thumb_w {
            let sx = (x * 90) / thumb_w;
            let sy = (y * 130) / thumb_h;
            buffer[(thumb_y + y) * CANVAS_WIDTH + (thumb_x + x)] = rom.preview[sy * 90 + sx];
        }
    }
    draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, thumb_x - 1, thumb_y - 1, thumb_w + 2, thumb_h + 2, 0x2A3B5C);

    // Painel de Estatísticas à direita
    let stat_x = mx + 102;
    let mut stat_y = my + 44;

    draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "INFORMACOES DO JOGO", stat_x, stat_y, 0x00E5FF);
    draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, stat_x, stat_y + 11, 230, 1, 0x1E2B45);
    stat_y += 20;

    // Tempo Jogado
    let hours = rom.stats.play_time_seconds / 3600;
    let minutes = (rom.stats.play_time_seconds % 3600) / 60;
    let time_str = format!("TEMPO JOGADO: {}h {}min", hours, minutes);
    draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, &time_str, stat_x, stat_y, 0xFFFFFF);
    stat_y += 18;

    // Vezes Aberto
    let sessions_str = format!("VEZES INICIADO: {}x", rom.stats.play_count);
    draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, &sessions_str, stat_x, stat_y, 0xCFD8DC);
    stat_y += 18;

    // Tipo de Hardware da ROM
    let hw_str = format!("MAPPER {} | {}KB PRG", rom.mapper, rom.prg_kb);
    draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, &hw_str, stat_x, stat_y, 0x78909C);

    // Botões de Ação no Rodapé do Card
    let btn_y = my + 148;
    let options = ["JOGAR", "RESETAR", "VOLTAR"];

    for (idx, opt) in options.iter().enumerate() {
        let bx = mx + 20 + idx * 110;
        let bw = 95;
        let is_sel = idx == card_cursor;

        let bg = if is_sel { 0x00E5FF } else { 0x141E30 };
        let text_c = if is_sel { 0x000000 } else { 0xCFD8DC };

        draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, bx, btn_y, bw, 22, bg);
        let tx = bx + (bw.saturating_sub(opt.len() * 6)) / 2;
        draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, opt, tx, btn_y + 7, text_c);
    }

    // Prompts do rodapé
    let hint_y = my + mh - 14;
    if gamepad_connected {
        draw_button_badge(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "< >", mx + 16, hint_y - 2, 0x37474F, 0xFFFFFF);
        draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "MUDAR", mx + 46, hint_y, 0x90A4AE);

        draw_button_badge(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "A", mx + 95, hint_y - 2, 0x2E7D32, 0xFFFFFF);
        draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "CONFIRMAR", mx + 112, hint_y, 0x90A4AE);

        draw_button_badge(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "B", mx + 185, hint_y - 2, 0xC62828, 0xFFFFFF);
        draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "FECHAR", mx + 202, hint_y, 0x90A4AE);
    } else {
        draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "< >: ESCOLHER   ENTER: CONFIRMAR   ESC: FECHAR", mx + 25, hint_y, 0x78909C);
    }
}

// ============================================================================
// NOVO MENU DE PAUSE IN-GAME COM ABAS (IGUAL AO HUB)
// ============================================================================
fn render_in_game_menu(
    buffer: &mut [u32],
    settings: &EmulatorSettings,
    rom_title: &str,
    has_save: bool,
    active_tab: usize,       // 0: PARTIDA, 1: VIDEO, 2: AUDIO
    row_cursor: usize,       // Linha selecionada na aba
    gamepad_connected: bool,
) {
    // 1. Escurecimento translúcido do jogo ao fundo
    for px in buffer.iter_mut() {
        let p = *px;
        let r = (((p >> 16) & 0xFF) * 2) / 10;
        let g = (((p >> 8) & 0xFF) * 2) / 10;
        let b = ((p & 0xFF) * 2) / 10;
        *px = (r << 16) | (g << 8) | b;
    }

    let mw = 380;
    let mh = 220;
    let mx = (CANVAS_WIDTH - mw) / 2;
    let my = (CANVAS_HEIGHT - mh) / 2;

    // Moldura principal com borda neon
    draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, mx - 1, my - 1, mw + 2, mh + 2, 0x00E5FF);
    draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, mx, my, mw, mh, 0x0B0F19);

    // Barra de Cabeçalho / Abas
    draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, mx, my, mw, 26, 0x131A29);
    draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, mx, my + 26, mw, 1, 0x1E2B42);

    let tabs = ["1.PARTIDA", "2.VIDEO", "3.AUDIO"];
    let tab_w = mw / tabs.len();

    for (t_idx, &t_name) in tabs.iter().enumerate() {
        let tx = mx + t_idx * tab_w;
        let is_tab_sel = t_idx == active_tab;

        if is_tab_sel {
            draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, tx, my + 1, tab_w, 25, 0x1D2A42);
            draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, tx, my + 24, tab_w, 2, 0x00E5FF);
        }
        let tc = if is_tab_sel { 0x00E5FF } else { 0x607D8B };
        let text_x = tx + (tab_w.saturating_sub(t_name.len() * 6)) / 2;
        draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, t_name, text_x, my + 9, tc);
    }

    // Sub-cabeçalho com título limpo do jogo
    let clean_title = sanitize_rom_name_for_scraping(rom_title);
    let title_short = if clean_title.len() > 36 {
        format!("{}...", &clean_title[..33])
    } else {
        clean_title
    };
    let sub_y = my + 30;
    let title_x = mx + (mw.saturating_sub(title_short.len() * 6)) / 2;
    draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, &title_short, title_x, sub_y, 0x00E5FF);
    draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, mx + 16, sub_y + 11, mw - 32, 1, 0x1E2B42);

    // Itens de cada Aba
    let items: Vec<(String, String)> = match active_tab {
        0 => vec![
            ("CONTINUAR JOGANDO".into(), "".into()),
            (format!("SALVAR ESTADO [SLOT {}]", settings.active_save_slot), "[F5]".into()),
            (format!("CARREGAR ESTADO [{}]", settings.active_save_slot), if has_save { "< PRONTO > [F8]".into() } else { "< VAZIO >".into() }),
            ("TROCAR SLOT ATIVO".into(), format!("< SLOT {} >", settings.active_save_slot)),
            ("REINICIAR CONSOLE".into(), "< RESET > [F2]".into()),
            ("SAIR PARA O HUB ARCADE".into(), "".into()),
        ],
        1 => vec![
            ("MODO DE TELA".into(), match settings.display_mode {
                DisplayMode::Windowed => "< JANELA (1024x576) >".into(),
                DisplayMode::Borderless => "< SEM BORDAS >".into(),
                DisplayMode::Fullscreen => "< TELA CHEIA >".into(),
            }),
            ("FORMATO DE TELA".into(), settings.aspect_label().into()),
            ("FILTRO SCANLINES".into(), if settings.crt_scanlines { "< ATIVADO >".into() } else { "< DESATIVADO >".into() }),
            ("LIMITE 8 SPRITES".into(), if settings.sprite_limit { "< ORIGINAL (FLICKER) >".into() } else { "< SEM LIMITE >".into() }),
        ],
        // ABA 2: ÁUDIO LIMPO E DIRETO
        2 => vec![
            ("VOLUME GERAL".into(), format!("< {}% >", (settings.volume * 100.0).round() as usize)),
            ("MUTAR AUDIO (MUTE)".into(), if settings.mute_all { "< ATIVADO >".into() } else { "< DESATIVADO >".into() }),
        ],
        _ => vec![],
    };

    let start_y = my + 48;
    let row_spacing = if active_tab == 0 { 22 } else { 30 };

    for (i, (label, val)) in items.iter().enumerate() {
        let iy = start_y + i * row_spacing;
        let is_sel = i == row_cursor;

        if is_sel {
            draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, mx + 12, iy - 3, mw - 24, if active_tab == 0 { 18 } else { 22 }, 0x16243D);
            draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, mx + 12, iy - 3, 3, if active_tab == 0 { 18 } else { 22 }, 0x00E5FF);
        }

        let l_color = if is_sel { 0xFFFFFF } else { 0x90A4AE };
        let v_color = if is_sel { 0x00E5FF } else { 0xCFD8DC };

        draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, label, mx + 22, iy + 2, l_color);
        if !val.is_empty() {
            let val_x = mx + mw - 22 - (val.len() * 6);
            draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, val, val_x, iy + 2, v_color);
        }
    }

    // Rodapé de Ações
    let foot_y = my + mh - 20;
    draw_rect(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, mx, foot_y - 4, mw, 1, 0x1A253A);

    if gamepad_connected {
        draw_button_badge(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "LB/RB", mx + 14, foot_y, 0x37474F, 0xFFFFFF);
        draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "ABAS", mx + 50, foot_y + 2, 0x90A4AE);

        draw_button_badge(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "A", mx + 95, foot_y, 0x2E7D32, 0xFFFFFF);
        draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "CONFIRMAR", mx + 112, foot_y + 2, 0x90A4AE);

        draw_button_badge(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "B", mx + 185, foot_y, 0xC62828, 0xFFFFFF);
        draw_text(buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "VOLTAR", mx + 202, foot_y + 2, 0x90A4AE);
    } else {
        draw_text(
            buffer,
            CANVAS_WIDTH,
            CANVAS_HEIGHT,
            "1-3: ABAS   CIMA/BAIXO: ITENS   ENTER: SELECIONAR   ESC: VOLTAR",
            mx + 10,
            foot_y + 2,
            0x78909C,
        );
    }
}

// ============================================================================
// ESCALONADOR DO NES PARA O CANVAS DE 512x288
// ============================================================================
fn blit_nes_to_canvas(
    nes_frame: &[u32; 256 * 240],
    canvas: &mut [u32],
    aspect_mode: u8,
    crt_scanlines: bool,
) {
    match aspect_mode {
        // 4:3 clássico ou imagem 4:3 com luz ambiente nas laterais.
        0 | 1 => {
            let target_w = 384;
            let offset_x = (CANVAS_WIDTH - target_w) / 2;

            for y in 0..CANVAS_HEIGHT {
                let row = y * CANVAS_WIDTH;
                if aspect_mode == 0 {
                    canvas[row..row + offset_x].fill(0x080A10);
                    canvas[row + offset_x + target_w..row + CANVAS_WIDTH].fill(0x080A10);
                } else {
                    let blur_y = ((y / 8) * 8 * 240 / CANVAS_HEIGHT).clamp(4, 235);
                    for x in 0..offset_x {
                        let sample_x = 8 + ((offset_x - 1 - x) * 32) / offset_x;
                        let p1 = nes_frame[(blur_y - 4) * 256 + sample_x];
                        let p2 = nes_frame[blur_y * 256 + (sample_x + 4).min(255)];
                        let p3 = nes_frame[(blur_y + 4) * 256 + sample_x];
                        let r = (((((p1 >> 16) & 0xFF) + ((p2 >> 16) & 0xFF) + ((p3 >> 16) & 0xFF)) / 10).min(255)) as u32;
                        let g = (((((p1 >> 8) & 0xFF) + ((p2 >> 8) & 0xFF) + ((p3 >> 8) & 0xFF)) / 10).min(255)) as u32;
                        let b = ((((p1 & 0xFF) + (p2 & 0xFF) + (p3 & 0xFF)) / 10).min(255)) as u32;
                        canvas[row + x] = (r << 16) | (g << 8) | b;
                    }
                    for x in (offset_x + target_w)..CANVAS_WIDTH {
                        let rel_x = x - (offset_x + target_w);
                        let sample_x = 247usize.saturating_sub((rel_x * 32) / offset_x);
                        let p1 = nes_frame[(blur_y - 4) * 256 + sample_x];
                        let p2 = nes_frame[blur_y * 256 + sample_x.saturating_sub(4)];
                        let p3 = nes_frame[(blur_y + 4) * 256 + sample_x];
                        let r = ((((p1 >> 16) & 0xFF) + ((p2 >> 16) & 0xFF) + ((p3 >> 16) & 0xFF)) / 10).min(255);
                        let g = ((((p1 >> 8) & 0xFF) + ((p2 >> 8) & 0xFF) + ((p3 >> 8) & 0xFF)) / 10).min(255);
                        let b = (((p1 & 0xFF) + (p2 & 0xFF) + (p3 & 0xFF)) / 10).min(255);
                        canvas[row + x] = (r << 16) | (g << 8) | b;
                    }
                    canvas[row + offset_x - 2] = 0x182438;
                    canvas[row + offset_x - 1] = 0x000000;
                    canvas[row + offset_x + target_w] = 0x000000;
                    canvas[row + offset_x + target_w + 1] = 0x182438;
                }
            }

            for y in 0..CANVAS_HEIGHT {
                let nes_row = (y * 240 / CANVAS_HEIGHT) * 256;
                let canvas_row = y * CANVAS_WIDTH + offset_x;
                let dim = crt_scanlines && y % 2 == 1;
                for x in 0..target_w {
                    let mut pixel = nes_frame[nes_row + (x * 256 / target_w)];
                    if dim {
                        let r = (((pixel >> 16) & 0xFF) * 8) / 10;
                        let g = (((pixel >> 8) & 0xFF) * 8) / 10;
                        let b = ((pixel & 0xFF) * 8) / 10;
                        pixel = (r << 16) | (g << 8) | b;
                    }
                    canvas[canvas_row + x] = pixel;
                }
            }
        }
        // 16:9 zoom proporcional, cortando o overscan vertical.
        2 => {
            let crop_top = 18;
            let visible_height = 240 - crop_top * 2;
            for y in 0..CANVAS_HEIGHT {
                let nes_row = (crop_top + y * visible_height / CANVAS_HEIGHT) * 256;
                let canvas_row = y * CANVAS_WIDTH;
                let dim = crt_scanlines && y % 2 == 1;
                for x in 0..CANVAS_WIDTH {
                    let mut pixel = nes_frame[nes_row + (x * 256 / CANVAS_WIDTH)];
                    if dim {
                        let r = (((pixel >> 16) & 0xFF) * 8) / 10;
                        let g = (((pixel >> 8) & 0xFF) * 8) / 10;
                        let b = ((pixel & 0xFF) * 8) / 10;
                        pixel = (r << 16) | (g << 8) | b;
                    }
                    canvas[canvas_row + x] = pixel;
                }
            }
        }
        // 16:9 esticado.
        _ => {
            for y in 0..CANVAS_HEIGHT {
                let nes_row = (y * 240 / CANVAS_HEIGHT) * 256;
                let canvas_row = y * CANVAS_WIDTH;
                let dim = crt_scanlines && y % 2 == 1;
                for x in 0..CANVAS_WIDTH {
                    let mut pixel = nes_frame[nes_row + (x * 256 / CANVAS_WIDTH)];
                    if dim {
                        let r = (((pixel >> 16) & 0xFF) * 8) / 10;
                        let g = (((pixel >> 8) & 0xFF) * 8) / 10;
                        let b = ((pixel & 0xFF) * 8) / 10;
                        pixel = (r << 16) | (g << 8) | b;
                    }
                    canvas[canvas_row + x] = pixel;
                }
            }
        }
    }
}

fn save_battery_ram(rom_path: &str, cart: &mut Cartridge) {
    if cart.prg_ram_dirty {
        let sav_path = std::path::Path::new(rom_path).with_extension("sav");
        if fs::write(&sav_path, &cart.prg_ram).is_ok() {
            cart.prg_ram_dirty = false;
            println!("Save de bateria (.sav) gravado em: {:?}", sav_path);
        }
    }
}

fn load_battery_ram(rom_path: &str, cart: &mut Cartridge) {
    let sav_path = std::path::Path::new(rom_path).with_extension("sav");
    if let Ok(sav_bytes) = fs::read(&sav_path) {
        let len = sav_bytes.len().min(cart.prg_ram.len());
        cart.prg_ram[..len].copy_from_slice(&sav_bytes[..len]);
        println!("Save de bateria (.sav) carregado: {:?}", sav_path);
    }
}

// Guarda ROMs e seleção de pasta em Documentos, longe da Área de Trabalho.
fn roms_documents_dir() -> PathBuf {
    if let Some(one_drive) = env::var_os("OneDrive") {
        let documents = PathBuf::from(one_drive).join("Documents");
        if documents.is_dir() {
            return documents.join("PEngine");
        }
    }

    if let Some(profile) = env::var_os("USERPROFILE") {
        return PathBuf::from(profile).join("Documents").join("PEngine");
    }

    env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        .join("Documents")
        .join("PEngine")
}

fn init_roms_directory(args: &[String]) -> String {
    let app_documents_dir = roms_documents_dir();
    let default_roms_dir = app_documents_dir.join("roms");
    let _ = fs::create_dir_all(&default_roms_dir);

    if args.len() >= 2 && fs::metadata(&args[1]).map(|m| m.is_dir()).unwrap_or(false) {
        return args[1].clone();
    }

    if let Ok(entries) = fs::read_dir(&default_roms_dir) {
        let has_roms = entries.flatten().any(|entry| {
            entry.path().extension()
                .map(|ext| ext.to_string_lossy().eq_ignore_ascii_case("nes"))
                .unwrap_or(false)
        });
        if has_roms {
            return default_roms_dir.to_string_lossy().into_owned();
        }
    }

    let saved_dir_path = app_documents_dir.join(".rom_dir.cfg");
    if let Ok(saved_dir) = fs::read_to_string(&saved_dir_path) {
        let trimmed = saved_dir.trim();
        if !trimmed.is_empty() && fs::metadata(trimmed).map(|m| m.is_dir()).unwrap_or(false) {
            return trimmed.to_string();
        }
    }

    default_roms_dir.to_string_lossy().into_owned()
}

// ============================================================================
// MAIN LOOP E ATALHOS DE SISTEMA (HUB ARCADE + EMULADOR)
// ============================================================================
fn main() {
    let args: Vec<String> = env::args().collect();
    let mut current_rom_dir = init_roms_directory(&args);

    let mut rom_list = scan_roms_in_directory(&current_rom_dir);
    let cover_loader = AsyncCoverLoader::new();
    let mut search_query = String::new();
    let mut in_search_mode = false;
    let mut vkb_cursor = 0usize;
    let mut filtered_indices: Vec<usize> = (0..rom_list.len()).collect();
    let mut selected_rom_idx = 0usize;
    let mut hub_category = HubCategory::All;
    let mut settings = EmulatorSettings::load();
    let mut in_settings_menu = false;
    let mut settings_tab = 0usize;      // 0 = VIDEO, 1 = AUDIO, 2 = SISTEMA, 3 = CONTROLES
    let mut settings_row = 0usize;      // Linha selecionada

    let mut in_detail_card = false;
    let mut detail_cursor = 0usize;
    let mut game_start_time: Option<SystemTime> = None;
    let mut start_hold_frames = 0usize;

    // Menu Pause in-game
    let mut in_game_menu = false;
    let mut in_game_tab = 0usize;      // 0: PARTIDA, 1: VÍDEO, 2: ÁUDIO
    let mut in_game_cursor = 0usize;

    let (_stream, sample_rate, audio_buffer) = match init_audio() {
        Some((s, sr, b)) => {
            println!("Audio CPAL iniciado em {} Hz!", sr);
            (Some(s), sr, b)
        }
        None => {
            println!("Aviso: Dispositivo de som nao encontrado. Rodando sem audio.");
            (None, 44100.0, Arc::new(Mutex::new(VecDeque::new())))
        }
    };

    let mut cpu: Option<Cpu> = None;
    let mut active_rom_path = String::new();
    let mut active_game_rom_idx: Option<usize> = None;
    let mut save_slots: [Option<Cpu>; 3] = [None, None, None];
    let mut rewind_mgr = RewindManager::new();
    let mut in_hub = true;

    let mut osd_message: Option<(String, usize)> = None;

    let mut prev_joy_buttons = 0u8;
    let mut joy_hold_timer = 0usize;

    if args.len() >= 2 && args[1].to_lowercase().ends_with(".nes") {
        if let Ok(bytes) = fs::read(&args[1]) {
            if let Ok(mut cart) = Cartridge::from_ines(&bytes) {
                load_battery_ram(&args[1], &mut cart);
                let apu = Apu::new(sample_rate, Arc::clone(&audio_buffer));
                let bus = Bus::new(cart, apu);
                let mut c = Cpu::new(bus);
                c.reset();
                cpu = Some(c);
                active_rom_path = args[1].clone();
                let requested_path = fs::canonicalize(&args[1]).ok();
                active_game_rom_idx = rom_list.iter().position(|rom| {
                    if let Some(ref requested_path) = requested_path {
                        fs::canonicalize(&rom.path).map(|path| path == *requested_path).unwrap_or(false)
                    } else {
                        rom.path == args[1]
                    }
                });
                save_slots = [None, None, None];
                in_hub = false;
            }
        }
    }

    let mut canvas_buffer = vec![0u32; CANVAS_WIDTH * CANVAS_HEIGHT];

    let mut window = Window::new(
        "PEngine",
        CANVAS_WIDTH,
        CANVAS_HEIGHT,
        WindowOptions {
            scale: Scale::X2, // 1024x576 na tela, limpo e amplo
            resize: true,
            scale_mode: minifb::ScaleMode::AspectRatioStretch,
            ..WindowOptions::default()
        },
    )
    .expect("Nao foi possivel criar a janela");

    window.set_target_fps(60);
    if settings.display_mode != DisplayMode::Windowed {
        apply_window_mode(&window, settings.display_mode);
    }

    let mut prev_lb = false;
    let mut prev_rb = false;
    let mut prev_menu_button = false;
    let mut prev_x_btn = false;
    let mut prev_select_btn = false;
    let mut prev_lt_btn = false;

    while window.is_open() {
        // Atalho universal de Tela Cheia (F11 ou Alt + Enter)
        let alt_enter = (window.is_key_down(Key::LeftAlt) || window.is_key_down(Key::RightAlt))
            && window.is_key_pressed(Key::Enter, KeyRepeat::No);
        if window.is_key_pressed(Key::F11, KeyRepeat::No) || alt_enter {
            settings.display_mode = match settings.display_mode {
                DisplayMode::Windowed => DisplayMode::Fullscreen,
                _ => DisplayMode::Windowed,
            };
            apply_window_mode(&window, settings.display_mode);
            settings.save();
        }
        let xbox = poll_xbox_controller();
        let x_just_pressed = xbox.x_pressed && !prev_x_btn;
        prev_x_btn = xbox.x_pressed;
        let xbox_menu_just_pressed = xbox.menu_btn_pressed && !prev_menu_button;
        prev_menu_button = xbox.menu_btn_held;
        let joy = xbox.nes_buttons;
        let joy_just_pressed = joy & !prev_joy_buttons;
        let select_btn = (joy & (1 << 2)) != 0;
        let select_just_pressed = select_btn && !prev_select_btn;
        prev_select_btn = select_btn;
        let lt_just_pressed = xbox.lt_held && !prev_lt_btn;
        prev_lt_btn = xbox.lt_held;

        // NOVO: Detecta se LB ou RB foram clicados neste frame exato (Clique Único)
        let lb_just_pressed = xbox.lb_held && !prev_lb;
        let rb_just_pressed = xbox.rb_held && !prev_rb;
        prev_lb = xbox.lb_held;
        prev_rb = xbox.rb_held;

        if (joy & 0xC0) != 0 || (joy & 0x30) != 0 {
            joy_hold_timer += 1;
        } else {
            joy_hold_timer = 0;
        }
        let joy_auto_repeat = joy_hold_timer > 16 && (joy_hold_timer % 4 == 0);
        prev_joy_buttons = joy;

        let selected_actual_rom_idx = filtered_indices.get(selected_rom_idx).copied().unwrap_or(0);
        if in_hub {
            // Tecla F3 ou Botão START do Xbox
            let start_pressed = (joy_just_pressed & (1 << 3)) != 0;
            let f3_pressed = window.is_key_pressed(Key::F3, KeyRepeat::No);
            let toggle_settings = f3_pressed || start_pressed;

            // 1. SE O MENU ESTIVER ABERTO:
            if in_settings_menu {
                // TROCA DE ABAS COM CLIQUE ÚNICO (LB / RB ou Gatilhos LT / RT)
                if lb_just_pressed {
                    settings_tab = if settings_tab == 0 { 3 } else { settings_tab - 1 };
                    settings_row = 0;
                }
                if rb_just_pressed {
                    settings_tab = (settings_tab + 1) % 4;
                    settings_row = 0;
                }

                // Teclas de atalho no teclado (1, 2, 3, 4 ou Q / E)
                if window.is_key_pressed(Key::Q, KeyRepeat::No) || window.is_key_pressed(Key::Key1, KeyRepeat::No) {
                    settings_tab = 0; settings_row = 0;
                }
                if window.is_key_pressed(Key::Key2, KeyRepeat::No) { settings_tab = 1; settings_row = 0; }
                if window.is_key_pressed(Key::Key3, KeyRepeat::No) { settings_tab = 2; settings_row = 0; }
                if window.is_key_pressed(Key::E, KeyRepeat::No) || window.is_key_pressed(Key::Key4, KeyRepeat::No) {
                    settings_tab = 3; settings_row = 0;
                }

                // Se apertar F3, START ou B/ESC, fecha o menu e encerra o frame
                let b_pressed = (joy_just_pressed & (1 << 1)) != 0;
                let esc_pressed = window.is_key_pressed(Key::Escape, KeyRepeat::No);
                
                if toggle_settings || b_pressed || esc_pressed {
                    in_settings_menu = false;
                } else {
                    // Controles internos do Menu de Opções
                    let up = window.is_key_pressed(Key::Up, KeyRepeat::No) || (joy_just_pressed & (1 << 4)) != 0;
                    let down = window.is_key_pressed(Key::Down, KeyRepeat::No) || (joy_just_pressed & (1 << 5)) != 0;
                    let left = window.is_key_pressed(Key::Left, KeyRepeat::No) || (joy_just_pressed & (1 << 6)) != 0;
                    let right = window.is_key_pressed(Key::Right, KeyRepeat::No) || (joy_just_pressed & (1 << 7)) != 0;

                    if (joy_just_pressed & (1 << 2)) != 0 { // Botão Select/Back também avança aba
                        settings_tab = (settings_tab + 1) % 4;
                        settings_row = 0;
                    }

                    let max_settings_rows = match settings_tab {
                        0 => 3, // Vídeo: 4 opções (0 a 3)
                        1 => 1, // Áudio: 2 opções (0 a 1)
                        2 => 2, // Sistema: 3 opções (0 a 2)
                        _ => 2,
                    };
                    if up && settings_row > 0 { settings_row -= 1; }
                    if down && settings_row < max_settings_rows { settings_row += 1; }

                    // Alterar valores com Direita / Esquerda ou Botão A
                    let confirm = window.is_key_pressed(Key::Enter, KeyRepeat::No) || (joy_just_pressed & (1 << 0)) != 0;
                    if left || right || confirm {
                        match settings_tab {
                            0 => match settings_row {
                                0 => {
                                    settings.display_mode = if left { settings.display_mode.prev() } else { settings.display_mode.next() };
                                    apply_window_mode(&window, settings.display_mode);
                                }
                                1 => settings.aspect_mode = if left { (settings.aspect_mode + 3) % 4 } else { (settings.aspect_mode + 1) % 4 },
                                2 => settings.crt_scanlines = !settings.crt_scanlines,
                                3 => settings.sprite_limit = !settings.sprite_limit,
                                _ => {}
                            },
                            1 => match settings_row {
                                0 => {
                                    if left { settings.volume = (settings.volume - 0.05).clamp(0.0, 1.0); }
                                    if right || confirm { settings.volume = (settings.volume + 0.05).clamp(0.0, 1.0); }
                                }
                                1 => settings.mute_all = !settings.mute_all,
                                _ => {}
                            },
                            2 => match settings_row {
                                0 => settings.active_save_slot = if settings.active_save_slot >= 3 { 1 } else { settings.active_save_slot + 1 },
                                1 => settings.turbo_multiplier = if settings.turbo_multiplier >= 4 { 2 } else { settings.turbo_multiplier + 1 },
                                _ => {}
                            },
                            _ => {}
                        }
                        settings.save();
                    }
                }
            } 
            // 2. SE O MENU ESTIVER FECHADO E O JOGADOR APERTOU START / F3:
            else if toggle_settings {
                in_settings_menu = true;
                settings_row = 0;
            } 
            // 3. SE ESTIVER NO MODAL DE DETALHES DO JOGO:
            else if in_detail_card {
                // CONTROLE DO CARD DE DETALHES
                let left = window.is_key_pressed(Key::Left, KeyRepeat::No) || (joy_just_pressed & (1 << 6)) != 0;
                let right = window.is_key_pressed(Key::Right, KeyRepeat::No) || (joy_just_pressed & (1 << 7)) != 0;
                let confirm = window.is_key_pressed(Key::Enter, KeyRepeat::No)
                    || window.is_key_pressed(Key::Z, KeyRepeat::No)
                    || (joy_just_pressed & (1 << 0)) != 0;
                let cancel = window.is_key_pressed(Key::Escape, KeyRepeat::No)
                    || (joy_just_pressed & (1 << 1)) != 0; // Botão B

                if left && detail_cursor > 0 { detail_cursor -= 1; }
                if right && detail_cursor < 2 { detail_cursor += 1; }

                if cancel {
                    in_detail_card = false;
                } else if confirm {
                    match detail_cursor {
                        0 => {
                            // 1. INICIAR O JOGO
                            let target_rom = rom_list[selected_actual_rom_idx].path.clone();
                            if let Ok(bytes) = fs::read(&target_rom) {
                                if let Ok(mut cart) = Cartridge::from_ines(&bytes) {
                                    load_battery_ram(&target_rom, &mut cart);
                                    let apu = Apu::new(sample_rate, Arc::clone(&audio_buffer));
                                    let bus = Bus::new(cart, apu);
                                    let mut c = Cpu::new(bus);
                                    c.reset();
                                    cpu = Some(c);
                                    active_rom_path = target_rom;
                                    active_game_rom_idx = Some(selected_actual_rom_idx);
                                    save_slots = [None, None, None];
                                    rewind_mgr.clear();
                                    in_hub = false;
                                    in_detail_card = false;
                                    in_game_menu = false;
                                    osd_message = Some(("BOM JOGO!".to_string(), 60));

                                    // Contabiliza nova sessão e grava o instante inicial
                                    rom_list[selected_actual_rom_idx].stats.play_count += 1;
                                    rom_list[selected_actual_rom_idx].stats.last_played_epoch = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_secs();
                                    save_game_stats(&rom_list[selected_actual_rom_idx].name, &rom_list[selected_actual_rom_idx].stats);
                                    game_start_time = Some(SystemTime::now());
                                }
                            }
                        }
                        1 => {
                            // 2. RESETAR DADOS DO JOGO
                            rom_list[selected_actual_rom_idx].stats = GameStats::default();
                            save_game_stats(&rom_list[selected_actual_rom_idx].name, &rom_list[selected_actual_rom_idx].stats);
                        }
                        2 => {
                            // 3. VOLTAR
                            in_detail_card = false;
                        }
                        _ => {}
                    }
                }
            } else {
                let open_folder = window.is_key_pressed(Key::F1, KeyRepeat::No)
                    || (xbox.connected && xbox.y_pressed);

                if open_folder {
                    if let Some(folder) = open_folder_dialog() {
                        current_rom_dir = folder;
                        let saved_dir_path = roms_documents_dir().join(".rom_dir.cfg");
                        let _ = fs::write(saved_dir_path, &current_rom_dir);
                        rom_list = scan_roms_in_directory(&current_rom_dir);
                        search_query.clear();
                        filtered_indices = rebuild_filtered_list(&rom_list, hub_category, &search_query);
                        selected_rom_idx = 0;
                    }
                }

                if in_search_mode {
                    let up = window.is_key_pressed(Key::Up, KeyRepeat::No) || (joy_just_pressed & (1 << 4)) != 0;
                    let down = window.is_key_pressed(Key::Down, KeyRepeat::No) || (joy_just_pressed & (1 << 5)) != 0;
                    let left = window.is_key_pressed(Key::Left, KeyRepeat::No) || (joy_just_pressed & (1 << 6)) != 0;
                    let right = window.is_key_pressed(Key::Right, KeyRepeat::No) || (joy_just_pressed & (1 << 7)) != 0;
                    let confirm = (joy_just_pressed & (1 << 0)) != 0 || window.is_key_pressed(Key::Enter, KeyRepeat::No);
                    let close_search = window.is_key_pressed(Key::Escape, KeyRepeat::No)
                        || window.is_key_pressed(Key::F2, KeyRepeat::No)
                        || x_just_pressed;

                    if up && vkb_cursor >= 10 { vkb_cursor -= 10; }
                    if down && vkb_cursor + 10 < VIRTUAL_KB_CHARS.len() { vkb_cursor += 10; }
                    if left && vkb_cursor > 0 { vkb_cursor -= 1; }
                    if right && vkb_cursor + 1 < VIRTUAL_KB_CHARS.len() { vkb_cursor += 1; }

                    let mut query_changed = false;
                    if (joy_just_pressed & (1 << 1)) != 0 {
                        search_query.pop();
                        query_changed = true;
                    } else if confirm {
                        match VIRTUAL_KB_CHARS[vkb_cursor] {
                            "ESPACO" => { search_query.push(' '); query_changed = true; }
                            "APAGAR" => { search_query.pop(); query_changed = true; }
                            "LIMPAR" => { search_query.clear(); query_changed = true; }
                            "OK" => in_search_mode = false,
                            ch => if search_query.len() < 18 {
                                search_query.push_str(ch);
                                query_changed = true;
                            },
                        }
                    }

                    if window.is_key_pressed(Key::Backspace, KeyRepeat::Yes) {
                        search_query.pop();
                        query_changed = true;
                    }
                    if window.is_key_pressed(Key::Space, KeyRepeat::No) && search_query.len() < 18 {
                        search_query.push(' ');
                        query_changed = true;
                    }
                    let key_map = [
                        (Key::A, 'A'), (Key::B, 'B'), (Key::C, 'C'), (Key::D, 'D'), (Key::E, 'E'),
                        (Key::F, 'F'), (Key::G, 'G'), (Key::H, 'H'), (Key::I, 'I'), (Key::J, 'J'),
                        (Key::K, 'K'), (Key::L, 'L'), (Key::M, 'M'), (Key::N, 'N'), (Key::O, 'O'),
                        (Key::P, 'P'), (Key::Q, 'Q'), (Key::R, 'R'), (Key::S, 'S'), (Key::T, 'T'),
                        (Key::U, 'U'), (Key::V, 'V'), (Key::W, 'W'), (Key::X, 'X'), (Key::Y, 'Y'),
                        (Key::Z, 'Z'), (Key::Key0, '0'), (Key::Key1, '1'), (Key::Key2, '2'),
                        (Key::Key3, '3'), (Key::Key4, '4'), (Key::Key5, '5'), (Key::Key6, '6'),
                        (Key::Key7, '7'), (Key::Key8, '8'), (Key::Key9, '9'),
                    ];
                    for &(key, ch) in &key_map {
                        if window.is_key_pressed(key, KeyRepeat::No) && search_query.len() < 18 {
                            search_query.push(ch);
                            query_changed = true;
                        }
                    }

                    if query_changed {
                        filtered_indices = rebuild_filtered_list(&rom_list, hub_category, &search_query);
                        selected_rom_idx = 0;
                    }
                    if close_search { in_search_mode = false; }
                } else {
                    if select_just_pressed || window.is_key_pressed(Key::F, KeyRepeat::No) {
                        if let Some(&actual_idx) = filtered_indices.get(selected_rom_idx) {
                            rom_list[actual_idx].stats.is_favorite = !rom_list[actual_idx].stats.is_favorite;
                            save_game_stats(&rom_list[actual_idx].name, &rom_list[actual_idx].stats);
                            filtered_indices = rebuild_filtered_list(&rom_list, hub_category, &search_query);
                            if selected_rom_idx >= filtered_indices.len() { selected_rom_idx = filtered_indices.len().saturating_sub(1); }
                        }
                    }
                    if lt_just_pressed || window.is_key_pressed(Key::Tab, KeyRepeat::No) {
                        hub_category = hub_category.next();
                        filtered_indices = rebuild_filtered_list(&rom_list, hub_category, &search_query);
                        selected_rom_idx = 0;
                    }
                    if window.is_key_pressed(Key::F2, KeyRepeat::No) || x_just_pressed {
                        in_search_mode = true;
                    }

                    if !filtered_indices.is_empty() {
                    let left_pressed = window.is_key_pressed(Key::Left, KeyRepeat::Yes)
                        || (joy_just_pressed & (1 << 6)) != 0
                        || (joy_auto_repeat && (joy & (1 << 6)) != 0);
                    let right_pressed = window.is_key_pressed(Key::Right, KeyRepeat::Yes)
                        || (joy_just_pressed & (1 << 7)) != 0
                        || (joy_auto_repeat && (joy & (1 << 7)) != 0);
                    let play_pressed = window.is_key_pressed(Key::Enter, KeyRepeat::No)
                        || window.is_key_pressed(Key::Z, KeyRepeat::No)
                        || (joy_just_pressed & (1 << 0)) != 0; // Apenas Botão A ou Enter abre o card do jogo

                    if left_pressed && selected_rom_idx > 0 {
                        selected_rom_idx -= 1;
                    }
                    if right_pressed && selected_rom_idx + 1 < filtered_indices.len() {
                        selected_rom_idx += 1;
                    }

                    let jump_next = rb_just_pressed || window.is_key_pressed(Key::PageDown, KeyRepeat::No);
                    let jump_prev = lb_just_pressed || window.is_key_pressed(Key::PageUp, KeyRepeat::No);
                    if jump_next && !filtered_indices.is_empty() {
                        let current = roms_char(&rom_list, &filtered_indices, selected_rom_idx);
                        selected_rom_idx = (selected_rom_idx + 1..filtered_indices.len())
                            .find(|&pos| rom_list[filtered_indices[pos]].first_char != current)
                            .unwrap_or(0);
                    }
                    if jump_prev && !filtered_indices.is_empty() {
                        let current = roms_char(&rom_list, &filtered_indices, selected_rom_idx);
                        if let Some(prev_diff) = (0..selected_rom_idx).rev()
                            .find(|&pos| rom_list[filtered_indices[pos]].first_char != current)
                        {
                            let target_char = rom_list[filtered_indices[prev_diff]].first_char;
                            selected_rom_idx = (0..=prev_diff)
                                .find(|&pos| rom_list[filtered_indices[pos]].first_char == target_char)
                                .unwrap_or(0);
                        } else {
                            selected_rom_idx = filtered_indices.len() - 1;
                        }
                    }

                    if play_pressed && !filtered_indices.is_empty() {
                        in_detail_card = true;
                        detail_cursor = 0;
                    }
                    }
                }
            }

            let mut visible_rom_indices = Vec::new();
            for offset in -3i32..=3 {
                let pos = selected_rom_idx as i32 + offset;
                if pos >= 0 && (pos as usize) < filtered_indices.len() {
                    visible_rom_indices.push(filtered_indices[pos as usize]);
                }
            }
            cover_loader.poll_and_request(&mut rom_list, &visible_rom_indices);

            render_arcade_hub_frame(
                &mut canvas_buffer,
                &rom_list,
                &filtered_indices,
                selected_rom_idx,
                hub_category,
                xbox.connected,
                in_settings_menu,
                in_search_mode,
                &search_query,
                vkb_cursor,
            );

            // 2. Se o card de detalhes estiver aberto, desenha por cima
            if in_detail_card && !filtered_indices.is_empty() {
                render_game_detail_card(&mut canvas_buffer, &rom_list[selected_actual_rom_idx], xbox.connected, detail_cursor);
            }

            // 3. Se o menu de opções estiver aberto, desenha por cima de tudo
            if in_settings_menu {
                render_advanced_settings_menu(
                    &mut canvas_buffer,
                    &settings,
                    settings_tab,
                    settings_row,
                    xbox.connected,
                );
            }

            window.update_with_buffer(&canvas_buffer, CANVAS_WIDTH, CANVAS_HEIGHT).unwrap();
        } else {
            // MODO DE JOGO ATIVO
            if let Some(ref mut c) = cpu {
                // Se mute_all for true, volume é 0.0; senão, usa o volume configurado
                c.bus.apu.master_volume = if settings.mute_all { 0.0 } else { settings.volume };
                c.bus.ppu.sprite_limit_enabled = settings.sprite_limit;

                // 1. Contador de segurar o botão Start no Gamepad
                let start_is_held = (joy & (1 << 3)) != 0; // Bit do Start segurado
                if start_is_held {
                    start_hold_frames += 1;
                } else {
                    start_hold_frames = 0;
                }

                // Dispara o menu ao segurar Start por ~45 frames ou pressionar ESC / Menu.
                let hold_triggered = start_hold_frames == 45;
                let esc_pressed = window.is_key_pressed(Key::Escape, KeyRepeat::No);
                let toggle_menu = esc_pressed || xbox_menu_just_pressed || hold_triggered;

                if in_game_menu {
                    // 1. Navegação entre Abas com LB/RB (clique único) ou teclas 1, 2, 3
                    if lb_just_pressed {
                        in_game_tab = if in_game_tab == 0 { 2 } else { in_game_tab - 1 };
                        in_game_cursor = 0;
                    }
                    if rb_just_pressed {
                        in_game_tab = (in_game_tab + 1) % 3;
                        in_game_cursor = 0;
                    }
                    if window.is_key_pressed(Key::Key1, KeyRepeat::No) { in_game_tab = 0; in_game_cursor = 0; }
                    if window.is_key_pressed(Key::Key2, KeyRepeat::No) { in_game_tab = 1; in_game_cursor = 0; }
                    if window.is_key_pressed(Key::Key3, KeyRepeat::No) { in_game_tab = 2; in_game_cursor = 0; }

                    let up = window.is_key_pressed(Key::Up, KeyRepeat::No) || (joy_just_pressed & (1 << 4)) != 0;
                    let down = window.is_key_pressed(Key::Down, KeyRepeat::No) || (joy_just_pressed & (1 << 5)) != 0;
                    let left = window.is_key_pressed(Key::Left, KeyRepeat::No) || (joy_just_pressed & (1 << 6)) != 0;
                    let right = window.is_key_pressed(Key::Right, KeyRepeat::No) || (joy_just_pressed & (1 << 7)) != 0;

                    let select = window.is_key_pressed(Key::Enter, KeyRepeat::No)
                        || window.is_key_pressed(Key::Z, KeyRepeat::No)
                        || (joy_just_pressed & (1 << 0)) != 0; // Enter ou (A)

                    let back = toggle_menu
                        || (joy_just_pressed & (1 << 1)) != 0; // ESC ou (B)

                    let max_rows = match in_game_tab {
                        0 => 5, // Aba 0 (Partida) tem 6 opções (0 a 5)
                        1 => 3, // Aba 1 (Vídeo) tem 4 opções (0 a 3)
                        2 => 1, // Aba 2 (Áudio) tem 2 opções (0 a 1)
                        _ => 2,
                    };
                    if up && in_game_cursor > 0 { in_game_cursor -= 1; }
                    if down && in_game_cursor < max_rows { in_game_cursor += 1; }

                    if back {
                        in_game_menu = false;
                        start_hold_frames = 0;
                    } else {
                        match in_game_tab {
                            // --- ABA 0: PARTIDA ---
                            0 => {
                                if select || (in_game_cursor == 3 && (left || right)) {
                                    match in_game_cursor {
                                        0 => in_game_menu = false, // Continuar
                                        1 => { // Salvar Estado
                                            save_battery_ram(&active_rom_path, &mut c.bus.cart);
                                            let slot_idx = settings.active_save_slot - 1;
                                            if c.save_state_to_disk(&active_rom_path, settings.active_save_slot) {
                                                save_slots[slot_idx] = Some(c.clone());
                                                osd_message = Some((format!("SALVO NO DISCO [SLOT {}]", settings.active_save_slot), 75));
                                                in_game_menu = false;
                                            } else { osd_message = Some(("ERRO AO SALVAR ESTADO".to_string(), 75)); }
                                        }
                                        2 => { // Carregar Estado
                                            let slot_idx = settings.active_save_slot - 1;
                                            if c.load_state_from_disk(&active_rom_path, settings.active_save_slot) {
                                                rewind_mgr.clear();
                                                osd_message = Some((format!("CARREGADO DO DISCO [SLOT {}]", settings.active_save_slot), 75));
                                                in_game_menu = false;
                                            } else if let Some(ref s) = save_slots[slot_idx] {
                                                *c = s.clone();
                                                osd_message = Some((format!("CARREGADO SLOT {}", settings.active_save_slot), 75));
                                                in_game_menu = false;
                                            } else {
                                                osd_message = Some((format!("SLOT {} VAZIO!", settings.active_save_slot), 60));
                                            }
                                        }
                                        3 => { // Trocar Slot
                                            if left {
                                                settings.active_save_slot = if settings.active_save_slot <= 1 { 3 } else { settings.active_save_slot - 1 };
                                            } else {
                                                settings.active_save_slot = if settings.active_save_slot >= 3 { 1 } else { settings.active_save_slot + 1 };
                                            }
                                        settings.save();
                                        }
                                        4 => { // Reset
                                            c.reset();
                                            osd_message = Some(("CONSOLE REINICIADO".to_string(), 60));
                                            in_game_menu = false;
                                        }
                                        5 => { // Sair pro Hub
                                            save_battery_ram(&active_rom_path, &mut c.bus.cart);
                                            if let Some(start) = game_start_time.take() {
                                                if let Ok(elapsed) = start.elapsed() {
                                                    if let Some(rom_idx) = active_game_rom_idx {
                                                        rom_list[rom_idx].stats.play_time_seconds += elapsed.as_secs();
                                                        save_game_stats(&rom_list[rom_idx].name, &rom_list[rom_idx].stats);
                                                    }
                                                }
                                            }
                                            active_game_rom_idx = None;
                                            in_hub = true;
                                            in_game_menu = false;
                                            continue;
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            // --- ABA 1: VÍDEO ---
                            1 => {
                                if left || right || select {
                                    match in_game_cursor {
                                        0 => {
                                            settings.display_mode = if left { settings.display_mode.prev() } else { settings.display_mode.next() };
                                            apply_window_mode(&window, settings.display_mode);
                                        }
                                        1 => settings.aspect_mode = if left { (settings.aspect_mode + 3) % 4 } else { (settings.aspect_mode + 1) % 4 },
                                        2 => settings.crt_scanlines = !settings.crt_scanlines,
                                        3 => settings.sprite_limit = !settings.sprite_limit,
                                        _ => {}
                                    }
                                    settings.save();
                                }
                            }
                            // --- ABA 2: ÁUDIO ---
                            2 => {
                                match in_game_cursor {
                                    0 => { // Volume Geral (passos de 5% com setas ou botão A)
                                        if left {
                                            settings.volume = (settings.volume - 0.05).clamp(0.0, 1.0);
                                        }
                                        if right || select {
                                            settings.volume = (settings.volume + 0.05).clamp(0.0, 1.0);
                                        }
                                        if left || right || select {
                                            settings.save();
                                        }
                                    }
                                    1 => { // Mutar Tudo (Liga / Desliga)
                                        if left || right || select {
                                            settings.mute_all = !settings.mute_all;
                                            settings.save();
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            _ => {}
                        }
                    }

                    // Renderiza o frame do jogo pausado com o novo menu em abas
                    blit_nes_to_canvas(&c.bus.ppu.framebuffer, &mut canvas_buffer, settings.aspect_mode, settings.crt_scanlines);
                    let has_slot_save = has_save_state_on_disk(&active_rom_path, settings.active_save_slot) || save_slots[settings.active_save_slot - 1].is_some();
                    let display_game_name = std::path::Path::new(&active_rom_path)
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| active_rom_path.clone());

                    render_in_game_menu(
                        &mut canvas_buffer,
                        &settings,
                        &display_game_name,
                        has_slot_save,
                        in_game_tab,
                        in_game_cursor,
                        xbox.connected,
                    );
                    window.update_with_buffer(&canvas_buffer, CANVAS_WIDTH, CANVAS_HEIGHT).unwrap();
                    continue;
                }

                // Abre o menu em um frame separado para não consumir o mesmo input como ação.
                else if toggle_menu {
                    in_game_menu = true;
                    in_game_tab = 0;
                    in_game_cursor = 0;
                    start_hold_frames = 0;
                    continue;
                }

                // Atalhos rápidos no teclado
                if window.is_key_pressed(Key::F2, KeyRepeat::No) {
                    c.reset();
                    osd_message = Some(("CONSOLE REINICIADO (F2)".to_string(), 60));
                }

                if window.is_key_pressed(Key::F5, KeyRepeat::No) {
                    save_battery_ram(&active_rom_path, &mut c.bus.cart);
                    let slot_idx = settings.active_save_slot - 1;
                    if c.save_state_to_disk(&active_rom_path, settings.active_save_slot) {
                        save_slots[slot_idx] = Some(c.clone());
                        osd_message = Some((format!("SALVO NO DISCO [SLOT {}] (F5)", settings.active_save_slot), 75));
                    } else { osd_message = Some(("ERRO AO SALVAR ESTADO".to_string(), 75)); }
                }

                if window.is_key_pressed(Key::F8, KeyRepeat::No) {
                    let slot_idx = settings.active_save_slot - 1;
                    if c.load_state_from_disk(&active_rom_path, settings.active_save_slot) {
                        rewind_mgr.clear();
                        osd_message = Some((format!("CARREGADO DO DISCO [SLOT {}] (F8)", settings.active_save_slot), 75));
                    } else if let Some(ref s) = save_slots[slot_idx] {
                        *c = s.clone();
                        osd_message = Some((format!("CARREGADO SLOT {} (F8)", settings.active_save_slot), 75));
                    } else {
                        osd_message = Some((format!("SLOT {} VAZIO (USE F5)", settings.active_save_slot), 75));
                    }
                }

                // Atualiza o controle antes de avançar a CPU para o jogo ler a entrada neste frame.
                let mut joy_state = 0u8;
                if window.is_key_down(Key::Z)          { joy_state |= 1 << 0; }
                if window.is_key_down(Key::X)          { joy_state |= 1 << 1; }
                if window.is_key_down(Key::RightShift) { joy_state |= 1 << 2; }
                if window.is_key_down(Key::Enter)      { joy_state |= 1 << 3; }
                if window.is_key_down(Key::Up)         { joy_state |= 1 << 4; }
                if window.is_key_down(Key::Down)       { joy_state |= 1 << 5; }
                if window.is_key_down(Key::Left)       { joy_state |= 1 << 6; }
                if window.is_key_down(Key::Right)      { joy_state |= 1 << 7; }
                joy_state |= joy;
                if xbox.y_pressed { joy_state |= 1 << 0; } // Y como A dentro do jogo
                if xbox.x_pressed { joy_state |= 1 << 1; } // X como B dentro do jogo
                c.bus.joypad1.status = joy_state;

                let rewind_active = xbox.lt_held || window.is_key_down(Key::Backspace);
                let turbo_active = (window.is_key_down(Key::Tab) || xbox.rb_held || xbox.rt_held) && !rewind_active;
                if rewind_active {
                    if let Some(previous) = rewind_mgr.pop_rewind_frame() { *c = previous; }
                } else {
                    let frames_to_run = if turbo_active { settings.turbo_multiplier } else { 1 };
                    for _ in 0..frames_to_run {
                        while !c.bus.ppu.frame_complete { c.step(); }
                        c.bus.ppu.frame_complete = false;
                    }
                    rewind_mgr.record_frame(c);
                }

                // Desenha o jogo na tela esticada ou com aspecto 4:3 com suporte a Scanlines
                blit_nes_to_canvas(
                    &c.bus.ppu.framebuffer,
                    &mut canvas_buffer,
                    settings.aspect_mode,
                    settings.crt_scanlines,
                );

                if turbo_active {
                    let turbo_str = format!("TURBO {}X", settings.turbo_multiplier);
                    draw_rect(&mut canvas_buffer, CANVAS_WIDTH, CANVAS_HEIGHT, CANVAS_WIDTH - 80, 8, 70, 16, 0x161C28);
                    draw_text(&mut canvas_buffer, CANVAS_WIDTH, CANVAS_HEIGHT, &turbo_str, CANVAS_WIDTH - 74, 12, 0xFFD54F);
                }
                if rewind_active {
                    draw_rect(&mut canvas_buffer, CANVAS_WIDTH, CANVAS_HEIGHT, CANVAS_WIDTH - 96, 8, 86, 16, 0x161C28);
                    draw_rect(&mut canvas_buffer, CANVAS_WIDTH, CANVAS_HEIGHT, CANVAS_WIDTH - 96, 8, 3, 16, 0x00E5FF);
                    draw_text(&mut canvas_buffer, CANVAS_WIDTH, CANVAS_HEIGHT, "<< REWIND", CANVAS_WIDTH - 86, 12, 0x00E5FF);
                }

                if let Some((ref text, ref mut frames_left)) = osd_message {
                    let box_w = (text.len() * 6 + 14).min(CANVAS_WIDTH - 20);
                    draw_rect(&mut canvas_buffer, CANVAS_WIDTH, CANVAS_HEIGHT, 10, 8, box_w, 16, 0x161C28);
                    draw_rect(&mut canvas_buffer, CANVAS_WIDTH, CANVAS_HEIGHT, 10, 8, 3, 16, 0x00E5FF);
                    draw_text(&mut canvas_buffer, CANVAS_WIDTH, CANVAS_HEIGHT, text, 18, 12, 0xFFFFFF);
                    if *frames_left > 0 {
                        *frames_left -= 1;
                    } else {
                        osd_message = None;
                    }
                }

                window
                    .update_with_buffer(&canvas_buffer, CANVAS_WIDTH, CANVAS_HEIGHT)
                    .unwrap();
            }
        }
    }

    if let Some(ref mut c) = cpu {
        save_battery_ram(&active_rom_path, &mut c.bus.cart);
    }
}
