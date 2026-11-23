use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use std::ffi::{CStr, c_char, c_int, c_void};
use std::fs::File;
use std::os::fd::FromRawFd;
use std::path::PathBuf;
use std::ptr;
use wayland_sys::client::{WaylandClient, wayland_client_option, wl_display, wl_proxy};
use wayland_sys::common::{wl_argument, wl_fixed_t, wl_interface, wl_message};
use winit::event_loop::EventLoopProxy;
use winit::window::Window;

use crate::app::UserEvent;

unsafe extern "C" {
    fn pipe(fds: *mut c_int) -> c_int;
    fn close(fd: c_int) -> c_int;
}

// Static Wayland protocol interface descriptors for wl_registry, wl_seat,
// wl_surface, wl_data_device_manager, wl_data_device, and wl_data_offer.
static NULL_TYPES_1: [*const wl_interface; 1] = [ptr::null()];
static NULL_TYPES_2: [*const wl_interface; 2] = [ptr::null(), ptr::null()];
static NULL_TYPES_3: [*const wl_interface; 3] = [ptr::null(), ptr::null(), ptr::null()];
static NULL_TYPES_4: [*const wl_interface; 4] = [ptr::null(), ptr::null(), ptr::null(), ptr::null()];

static REGISTRY_REQUESTS: [wl_message; 1] = [wl_message {
    name: c"bind".as_ptr(),
    signature: c"usun".as_ptr(),
    types: NULL_TYPES_4.as_ptr(),
}];
static REGISTRY_EVENTS: [wl_message; 2] = [
    wl_message {
        name: c"global".as_ptr(),
        signature: c"usu".as_ptr(),
        types: NULL_TYPES_3.as_ptr(),
    },
    wl_message {
        name: c"global_remove".as_ptr(),
        signature: c"u".as_ptr(),
        types: NULL_TYPES_1.as_ptr(),
    },
];
static WL_REGISTRY_INTERFACE: wl_interface = wl_interface {
    name: c"wl_registry".as_ptr(),
    version: 1,
    request_count: 1,
    requests: REGISTRY_REQUESTS.as_ptr(),
    event_count: 2,
    events: REGISTRY_EVENTS.as_ptr(),
};

static WL_SEAT_INTERFACE: wl_interface = wl_interface {
    name: c"wl_seat".as_ptr(),
    version: 1,
    request_count: 0,
    requests: ptr::null(),
    event_count: 0,
    events: ptr::null(),
};

static WL_SURFACE_INTERFACE: wl_interface = wl_interface {
    name: c"wl_surface".as_ptr(),
    version: 1,
    request_count: 0,
    requests: ptr::null(),
    event_count: 0,
    events: ptr::null(),
};

static DATA_OFFER_REQUESTS: [wl_message; 5] = [
    wl_message {
        name: c"accept".as_ptr(),
        signature: c"u?s".as_ptr(),
        types: NULL_TYPES_2.as_ptr(),
    },
    wl_message {
        name: c"receive".as_ptr(),
        signature: c"sh".as_ptr(),
        types: NULL_TYPES_2.as_ptr(),
    },
    wl_message {
        name: c"destroy".as_ptr(),
        signature: c"".as_ptr(),
        types: ptr::null(),
    },
    wl_message {
        name: c"finish".as_ptr(),
        signature: c"".as_ptr(),
        types: ptr::null(),
    },
    wl_message {
        name: c"set_actions".as_ptr(),
        signature: c"uu".as_ptr(),
        types: NULL_TYPES_2.as_ptr(),
    },
];
static DATA_OFFER_EVENTS: [wl_message; 3] = [
    wl_message {
        name: c"offer".as_ptr(),
        signature: c"s".as_ptr(),
        types: NULL_TYPES_1.as_ptr(),
    },
    wl_message {
        name: c"source_actions".as_ptr(),
        signature: c"u".as_ptr(),
        types: NULL_TYPES_1.as_ptr(),
    },
    wl_message {
        name: c"action".as_ptr(),
        signature: c"u".as_ptr(),
        types: NULL_TYPES_1.as_ptr(),
    },
];
static WL_DATA_OFFER_INTERFACE: wl_interface = wl_interface {
    name: c"wl_data_offer".as_ptr(),
    version: 3,
    request_count: 5,
    requests: DATA_OFFER_REQUESTS.as_ptr(),
    event_count: 3,
    events: DATA_OFFER_EVENTS.as_ptr(),
};

static DATA_DEVICE_OFFER_TYPES: [*const wl_interface; 1] = [&WL_DATA_OFFER_INTERFACE];
static DATA_DEVICE_ENTER_TYPES: [*const wl_interface; 5] = [
    ptr::null(),
    &WL_SURFACE_INTERFACE,
    ptr::null(),
    ptr::null(),
    &WL_DATA_OFFER_INTERFACE,
];
static DATA_DEVICE_EVENTS: [wl_message; 6] = [
    wl_message {
        name: c"data_offer".as_ptr(),
        signature: c"n".as_ptr(),
        types: DATA_DEVICE_OFFER_TYPES.as_ptr(),
    },
    wl_message {
        name: c"enter".as_ptr(),
        signature: c"uoff?o".as_ptr(),
        types: DATA_DEVICE_ENTER_TYPES.as_ptr(),
    },
    wl_message {
        name: c"leave".as_ptr(),
        signature: c"".as_ptr(),
        types: ptr::null(),
    },
    wl_message {
        name: c"motion".as_ptr(),
        signature: c"uff".as_ptr(),
        types: NULL_TYPES_3.as_ptr(),
    },
    wl_message {
        name: c"drop".as_ptr(),
        signature: c"".as_ptr(),
        types: ptr::null(),
    },
    wl_message {
        name: c"selection".as_ptr(),
        signature: c"?o".as_ptr(),
        types: DATA_DEVICE_OFFER_TYPES.as_ptr(),
    },
];
static WL_DATA_DEVICE_INTERFACE: wl_interface = wl_interface {
    name: c"wl_data_device".as_ptr(),
    version: 3,
    request_count: 0,
    requests: ptr::null(),
    event_count: 6,
    events: DATA_DEVICE_EVENTS.as_ptr(),
};

static DDM_GET_DATA_DEVICE_TYPES: [*const wl_interface; 2] =
    [&WL_DATA_DEVICE_INTERFACE, &WL_SEAT_INTERFACE];
static DDM_REQUESTS: [wl_message; 2] = [
    wl_message {
        name: c"create_data_source".as_ptr(),
        signature: c"n".as_ptr(),
        types: NULL_TYPES_1.as_ptr(),
    },
    wl_message {
        name: c"get_data_device".as_ptr(),
        signature: c"no".as_ptr(),
        types: DDM_GET_DATA_DEVICE_TYPES.as_ptr(),
    },
];
static WL_DATA_DEVICE_MANAGER_INTERFACE: wl_interface = wl_interface {
    name: c"wl_data_device_manager".as_ptr(),
    version: 3,
    request_count: 2,
    requests: DDM_REQUESTS.as_ptr(),
    event_count: 0,
    events: ptr::null(),
};

#[repr(C)]
struct WlRegistryListener {
    global: extern "C" fn(*mut c_void, *mut wl_proxy, u32, *const c_char, u32),
    global_remove: extern "C" fn(*mut c_void, *mut wl_proxy, u32),
}

#[repr(C)]
struct WlDataOfferListener {
    offer: extern "C" fn(*mut c_void, *mut wl_proxy, *const c_char),
    source_actions: extern "C" fn(*mut c_void, *mut wl_proxy, u32),
    action: extern "C" fn(*mut c_void, *mut wl_proxy, u32),
}

#[repr(C)]
struct WlDataDeviceListener {
    data_offer: extern "C" fn(*mut c_void, *mut wl_proxy, *mut wl_proxy),
    enter: extern "C" fn(
        *mut c_void,
        *mut wl_proxy,
        u32,
        *mut wl_proxy,
        wl_fixed_t,
        wl_fixed_t,
        *mut wl_proxy,
    ),
    leave: extern "C" fn(*mut c_void, *mut wl_proxy),
    motion: extern "C" fn(*mut c_void, *mut wl_proxy, u32, wl_fixed_t, wl_fixed_t),
    drop: extern "C" fn(*mut c_void, *mut wl_proxy),
    selection: extern "C" fn(*mut c_void, *mut wl_proxy, *mut wl_proxy),
}

static REGISTRY_LISTENER: WlRegistryListener = WlRegistryListener {
    global: on_registry_global,
    global_remove: on_registry_global_remove,
};

static DATA_OFFER_LISTENER: WlDataOfferListener = WlDataOfferListener {
    offer: on_offer_mime,
    source_actions: on_offer_source_actions,
    action: on_offer_action,
};

static DATA_DEVICE_LISTENER: WlDataDeviceListener = WlDataDeviceListener {
    data_offer: on_data_offer,
    enter: on_data_enter,
    leave: on_data_leave,
    motion: on_data_motion,
    drop: on_data_drop,
    selection: on_data_selection,
};

struct OfferState {
    has_uri_list: bool,
}

struct DndState {
    wl: &'static WaylandClient,
    display: *mut wl_display,
    target_surface: *mut wl_proxy,
    proxy: EventLoopProxy<UserEvent>,
    registry: *mut wl_proxy,
    ddm: *mut wl_proxy,
    ddm_version: u32,
    seat: *mut wl_proxy,
    data_device: *mut wl_proxy,
    current_offer: Option<(*mut wl_proxy, u32)>,
}

pub struct WaylandDnd {
    state: Box<DndState>,
}

impl WaylandDnd {
    pub fn attach(window: &Window, proxy: EventLoopProxy<UserEvent>) -> Option<Self> {
        let dh = window.display_handle().ok()?.as_raw();
        let wh = window.window_handle().ok()?.as_raw();
        let (RawDisplayHandle::Wayland(wd), RawWindowHandle::Wayland(ww)) = (dh, wh) else {
            return None;
        };

        let wl = wayland_client_option()?;
        let display = wd.display.as_ptr() as *mut wl_display;
        let target_surface = ww.surface.as_ptr() as *mut wl_proxy;

        let mut state = Box::new(DndState {
            wl,
            display,
            target_surface,
            proxy,
            registry: ptr::null_mut(),
            ddm: ptr::null_mut(),
            ddm_version: 0,
            seat: ptr::null_mut(),
            data_device: ptr::null_mut(),
            current_offer: None,
        });

        unsafe {
            let mut args = [wl_argument { o: ptr::null_mut() }];
            let registry = (wl.wl_proxy_marshal_array_constructor)(
                display as *mut wl_proxy,
                1, // wl_display::get_registry
                args.as_mut_ptr(),
                &WL_REGISTRY_INTERFACE,
            );
            if registry.is_null() {
                return None;
            }
            state.registry = registry;
            let state_ptr = &mut *state as *mut DndState as *mut c_void;
            (wl.wl_proxy_add_listener)(
                registry,
                &REGISTRY_LISTENER as *const WlRegistryListener as *mut extern "C" fn(),
                state_ptr,
            );
            (wl.wl_display_roundtrip)(display);

            if state.ddm.is_null() || state.seat.is_null() {
                return None;
            }

            let mut dd_args = [
                wl_argument { o: ptr::null_mut() },
                wl_argument {
                    o: state.seat as *mut c_void,
                },
            ];
            let data_device = (wl.wl_proxy_marshal_array_constructor_versioned)(
                state.ddm,
                1, // wl_data_device_manager::get_data_device
                dd_args.as_mut_ptr(),
                &WL_DATA_DEVICE_INTERFACE,
                state.ddm_version.min(3),
            );
            if data_device.is_null() {
                return None;
            }
            state.data_device = data_device;
            (wl.wl_proxy_add_listener)(
                data_device,
                &DATA_DEVICE_LISTENER as *const WlDataDeviceListener as *mut extern "C" fn(),
                state_ptr,
            );
            (wl.wl_display_flush)(display);
        }

        tracing::info!("Attached native Wayland wl_data_device drag-and-drop handler");
        Some(Self { state })
    }

    pub fn finish_offer(&self, offer_ptr: usize) {
        if offer_ptr == 0 {
            return;
        }
        let offer = offer_ptr as *mut wl_proxy;
        unsafe {
            if self.state.ddm_version >= 3 {
                (self.state.wl.wl_proxy_marshal_array)(offer, 3, ptr::null_mut()); // finish
            }
            (self.state.wl.wl_proxy_marshal_array)(offer, 2, ptr::null_mut()); // destroy
            (self.state.wl.wl_proxy_destroy)(offer);
            (self.state.wl.wl_display_flush)(self.state.display);
        }
    }
}

impl Drop for WaylandDnd {
    fn drop(&mut self) {
        unsafe {
            let wl = self.state.wl;
            if let Some((offer, _)) = self.state.current_offer.take() {
                free_offer(wl, offer);
            }
            if !self.state.data_device.is_null() {
                (wl.wl_proxy_destroy)(self.state.data_device);
            }
            if !self.state.seat.is_null() {
                (wl.wl_proxy_destroy)(self.state.seat);
            }
            if !self.state.ddm.is_null() {
                (wl.wl_proxy_destroy)(self.state.ddm);
            }
            if !self.state.registry.is_null() {
                (wl.wl_proxy_destroy)(self.state.registry);
            }
        }
    }
}

unsafe fn free_offer(wl: &WaylandClient, offer: *mut wl_proxy) {
    if offer.is_null() {
        return;
    }
    let ud = (wl.wl_proxy_get_user_data)(offer);
    if !ud.is_null() {
        (wl.wl_proxy_set_user_data)(offer, ptr::null_mut());
        drop(Box::from_raw(ud as *mut OfferState));
    }
    (wl.wl_proxy_marshal_array)(offer, 2, ptr::null_mut()); // destroy
    (wl.wl_proxy_destroy)(offer);
}

extern "C" fn on_registry_global(
    data: *mut c_void,
    registry: *mut wl_proxy,
    name: u32,
    interface: *const c_char,
    version: u32,
) {
    if data.is_null() || interface.is_null() {
        return;
    }
    unsafe {
        let state = &mut *(data as *mut DndState);
        let iface = CStr::from_ptr(interface).to_string_lossy();
        if iface == "wl_data_device_manager" && state.ddm.is_null() {
            let bind_ver = version.min(3);
            let mut args = [
                wl_argument { u: name },
                wl_argument {
                    s: WL_DATA_DEVICE_MANAGER_INTERFACE.name,
                },
                wl_argument { u: bind_ver },
                wl_argument { o: ptr::null_mut() },
            ];
            let proxy = (state.wl.wl_proxy_marshal_array_constructor_versioned)(
                registry,
                0, // wl_registry::bind
                args.as_mut_ptr(),
                &WL_DATA_DEVICE_MANAGER_INTERFACE,
                bind_ver,
            );
            if !proxy.is_null() {
                state.ddm = proxy;
                state.ddm_version = bind_ver;
            }
        } else if iface == "wl_seat" && state.seat.is_null() {
            let mut args = [
                wl_argument { u: name },
                wl_argument {
                    s: WL_SEAT_INTERFACE.name,
                },
                wl_argument { u: 1 },
                wl_argument { o: ptr::null_mut() },
            ];
            let proxy = (state.wl.wl_proxy_marshal_array_constructor_versioned)(
                registry,
                0,
                args.as_mut_ptr(),
                &WL_SEAT_INTERFACE,
                1,
            );
            if !proxy.is_null() {
                state.seat = proxy;
            }
        }
    }
}

extern "C" fn on_registry_global_remove(_data: *mut c_void, _registry: *mut wl_proxy, _name: u32) {}

extern "C" fn on_offer_mime(data: *mut c_void, _offer: *mut wl_proxy, mime_type: *const c_char) {
    if data.is_null() || mime_type.is_null() {
        return;
    }
    unsafe {
        let offer_state = &mut *(data as *mut OfferState);
        if CStr::from_ptr(mime_type).to_bytes() == b"text/uri-list" {
            offer_state.has_uri_list = true;
        }
    }
}

extern "C" fn on_offer_source_actions(_data: *mut c_void, _offer: *mut wl_proxy, _actions: u32) {}
extern "C" fn on_offer_action(_data: *mut c_void, _offer: *mut wl_proxy, _action: u32) {}

extern "C" fn on_data_offer(data: *mut c_void, _dev: *mut wl_proxy, offer: *mut wl_proxy) {
    if data.is_null() || offer.is_null() {
        return;
    }
    unsafe {
        let state = &mut *(data as *mut DndState);
        let offer_state = Box::into_raw(Box::new(OfferState {
            has_uri_list: false,
        }));
        (state.wl.wl_proxy_add_listener)(
            offer,
            &DATA_OFFER_LISTENER as *const WlDataOfferListener as *mut extern "C" fn(),
            offer_state as *mut c_void,
        );
    }
}

extern "C" fn on_data_enter(
    data: *mut c_void,
    _dev: *mut wl_proxy,
    serial: u32,
    surface: *mut wl_proxy,
    _x: wl_fixed_t,
    _y: wl_fixed_t,
    offer: *mut wl_proxy,
) {
    if data.is_null() {
        return;
    }
    unsafe {
        let state = &mut *(data as *mut DndState);
        if let Some((old, _)) = state.current_offer.take() {
            free_offer(state.wl, old);
        }
        if surface != state.target_surface || offer.is_null() {
            return;
        }
        let ud = (state.wl.wl_proxy_get_user_data)(offer);
        let has_uris = !ud.is_null() && (*(ud as *const OfferState)).has_uri_list;
        if !has_uris {
            free_offer(state.wl, offer);
            return;
        }

        state.current_offer = Some((offer, serial));
        let mut accept_args = [
            wl_argument { u: serial },
            wl_argument {
                s: c"text/uri-list".as_ptr(),
            },
        ];
        (state.wl.wl_proxy_marshal_array)(offer, 0, accept_args.as_mut_ptr());
        if state.ddm_version >= 3 {
            let mut act_args = [wl_argument { u: 7 }, wl_argument { u: 1 }];
            (state.wl.wl_proxy_marshal_array)(offer, 4, act_args.as_mut_ptr());
        }
        (state.wl.wl_display_flush)(state.display);
        let _ = state.proxy.send_event(UserEvent::WaylandHover(true));
    }
}

extern "C" fn on_data_leave(data: *mut c_void, _dev: *mut wl_proxy) {
    if data.is_null() {
        return;
    }
    unsafe {
        let state = &mut *(data as *mut DndState);
        if let Some((offer, _)) = state.current_offer.take() {
            free_offer(state.wl, offer);
        }
        let _ = state.proxy.send_event(UserEvent::WaylandHover(false));
    }
}

extern "C" fn on_data_motion(
    data: *mut c_void,
    _dev: *mut wl_proxy,
    _time: u32,
    _x: wl_fixed_t,
    _y: wl_fixed_t,
) {
    if data.is_null() {
        return;
    }
    unsafe {
        let state = &mut *(data as *mut DndState);
        if let Some((offer, serial)) = state.current_offer {
            let mut accept_args = [
                wl_argument { u: serial },
                wl_argument {
                    s: c"text/uri-list".as_ptr(),
                },
            ];
            (state.wl.wl_proxy_marshal_array)(offer, 0, accept_args.as_mut_ptr());
        }
    }
}

extern "C" fn on_data_drop(data: *mut c_void, _dev: *mut wl_proxy) {
    if data.is_null() {
        return;
    }
    unsafe {
        let state = &mut *(data as *mut DndState);
        let _ = state.proxy.send_event(UserEvent::WaylandHover(false));
        let Some((offer, _)) = state.current_offer.take() else {
            return;
        };

        // Free our OfferState box now; the wl_proxy itself will be finished & destroyed
        // on the main thread after the background pipe read finishes.
        let ud = (state.wl.wl_proxy_get_user_data)(offer);
        if !ud.is_null() {
            (state.wl.wl_proxy_set_user_data)(offer, ptr::null_mut());
            drop(Box::from_raw(ud as *mut OfferState));
        }

        let mut fds = [0 as c_int; 2];
        if pipe(fds.as_mut_ptr()) != 0 {
            (state.wl.wl_proxy_marshal_array)(offer, 2, ptr::null_mut());
            (state.wl.wl_proxy_destroy)(offer);
            return;
        }
        let read_fd = fds[0];
        let write_fd = fds[1];

        let mut recv_args = [
            wl_argument {
                s: c"text/uri-list".as_ptr(),
            },
            wl_argument { h: write_fd },
        ];
        (state.wl.wl_proxy_marshal_array)(offer, 1, recv_args.as_mut_ptr());
        close(write_fd);
        (state.wl.wl_display_flush)(state.display);

        let proxy = state.proxy.clone();
        let offer_addr = offer as usize;
        std::thread::spawn(move || {
            use std::io::Read;
            let mut file = File::from_raw_fd(read_fd);
            let mut buf = String::new();
            let _ = file.read_to_string(&mut buf);
            let path = parse_first_file_uri(&buf);
            let _ = proxy.send_event(UserEvent::WaylandDrop {
                path,
                offer_ptr: offer_addr,
            });
        });
    }
}

extern "C" fn on_data_selection(data: *mut c_void, _dev: *mut wl_proxy, offer: *mut wl_proxy) {
    if data.is_null() || offer.is_null() {
        return;
    }
    unsafe {
        let state = &*(data as *mut DndState);
        if state.current_offer.is_some_and(|(cur, _)| cur == offer) {
            return;
        }
        let ud = (state.wl.wl_proxy_get_user_data)(offer);
        if !ud.is_null() {
            (state.wl.wl_proxy_set_user_data)(offer, ptr::null_mut());
            drop(Box::from_raw(ud as *mut OfferState));
        }
        (state.wl.wl_proxy_destroy)(offer);
    }
}

pub fn parse_first_file_uri(uri_list: &str) -> Option<PathBuf> {
    for raw_line in uri_list.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let rest = if let Some(r) = line.strip_prefix("file://localhost") {
            r
        } else if let Some(r) = line.strip_prefix("file://") {
            r
        } else {
            continue;
        };
        if !rest.starts_with('/') {
            continue;
        }
        let decoded = percent_decode(rest);
        let p = PathBuf::from(decoded);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3])
            && let Ok(b) = u8::from_str_radix(hex, 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
