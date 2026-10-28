//! JNI functions loaded by the Kotlin app as `libgrassline_mobile.so`, behind
//! `net.grassline.app.RustBridge`. There is one controller per process.

use crate::Controller;
use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{JClass, JString};
use jni::sys::{jlong, jstring};
use jni::EnvUnowned;
use std::sync::OnceLock;
use std::time::Duration;

static CONTROLLER: OnceLock<Controller> = OnceLock::new();

fn controller() -> &'static Controller {
    CONTROLLER.get().expect("RustBridge.init was not called")
}

/// `init(filesDir: String): String`: opens the controller; returns this
/// device's key.
#[no_mangle]
pub extern "system" fn Java_net_grassline_app_RustBridge_init(
    mut env: EnvUnowned<'_>,
    _class: JClass,
    dir: JString,
) -> jstring {
    env.with_env(|env| -> jni::errors::Result<jstring> {
        let dir = dir.try_to_string(env)?;
        #[cfg(target_os = "android")]
        android_logger::init_once(
            android_logger::Config::default()
                .with_tag("grassline")
                .with_max_level(log::LevelFilter::Info),
        );
        if CONTROLLER.get().is_none() {
            match Controller::new(std::path::Path::new(&dir)) {
                Ok(c) => {
                    let _ = CONTROLLER.set(c);
                }
                Err(e) => {
                    let _ = env.throw(format!("cannot start: {e}"));
                    return Ok(std::ptr::null_mut());
                }
            }
        }
        Ok(env.new_string(controller().key())?.into_raw())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `snapshot(): String`: the state as JSON.
#[no_mangle]
pub extern "system" fn Java_net_grassline_app_RustBridge_snapshot(
    mut env: EnvUnowned<'_>,
    _class: JClass,
) -> jstring {
    env.with_env(|env| -> jni::errors::Result<jstring> {
        Ok(env.new_string(controller().snapshot())?.into_raw())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `await(seen: Long, timeoutMs: Long): Long`: blocks until the state is newer
/// than `seen`; returns its version. A negative `seen` returns it at once.
#[no_mangle]
pub extern "system" fn Java_net_grassline_app_RustBridge_await(
    _env: EnvUnowned<'_>,
    _class: JClass,
    seen: jlong,
    timeout_ms: jlong,
) -> jlong {
    let Ok(seen) = u64::try_from(seen) else {
        return controller().version() as jlong;
    };
    controller().wait(seen, Duration::from_millis(timeout_ms.max(0) as u64)) as jlong
}

/// `pair(ticket: String, deviceName: String): String?`: an error message, or null.
#[no_mangle]
pub extern "system" fn Java_net_grassline_app_RustBridge_pair(
    mut env: EnvUnowned<'_>,
    _class: JClass,
    ticket: JString,
    name: JString,
) -> jstring {
    env.with_env(|env| -> jni::errors::Result<jstring> {
        let (ticket, name) = (ticket.try_to_string(env)?, name.try_to_string(env)?);
        match controller().pair(&ticket, &name) {
            Ok(()) => Ok(std::ptr::null_mut()),
            Err(e) => Ok(env.new_string(e)?.into_raw()),
        }
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

macro_rules! simple {
    ($($name:ident($($arg:ident),*) => |$c:ident $(, $a:ident)*| $body:expr;)*) => {$(
        #[no_mangle]
        pub extern "system" fn $name(mut env: EnvUnowned<'_>, _class: JClass, $($arg: JString),*) {
            env.with_env(|env| -> jni::errors::Result<()> {
                $(let $arg = $arg.try_to_string(env)?;)*
                let $c = controller();
                $(let $a = $arg;)*
                $body;
                Ok(())
            })
            .resolve::<ThrowRuntimeExAndDefault>()
        }
    )*};
}

simple! {
    Java_net_grassline_app_RustBridge_create(project, provider) => |c, project, provider| c.create(&project, &provider);
    Java_net_grassline_app_RustBridge_setDefaultAgent(provider) => |c, provider| c.set_default_agent(&provider);
    Java_net_grassline_app_RustBridge_setAgent(id, provider) => |c, id, provider| c.set_agent(&id, &provider);
    Java_net_grassline_app_RustBridge_setModel(id, model) => |c, id, model| c.set_model(&id, &model);
    Java_net_grassline_app_RustBridge_setPermissions(id, permissions) => |c, id, permissions| {
        if let Err(e) = c.set_permissions(&id, &permissions) {
            log::warn!("bad permissions: {e}");
        }
    };
    Java_net_grassline_app_RustBridge_models(provider) => |c, provider| c.models(&provider);
    Java_net_grassline_app_RustBridge_open(id) => |c, id| c.open(&id);
    Java_net_grassline_app_RustBridge_stop(id) => |c, id| c.stop(&id);
    Java_net_grassline_app_RustBridge_send(id, text) => |c, id, text| c.send_text(&id, &text);
    Java_net_grassline_app_RustBridge_decide(id, request, decision) => |c, id, request, decision| {
        if let Err(e) = c.decide(&id, &request, &decision) {
            log::warn!("bad decision: {e}");
        }
    };
}

/// `refresh()`.
#[no_mangle]
pub extern "system" fn Java_net_grassline_app_RustBridge_refresh(
    _env: EnvUnowned<'_>,
    _class: JClass,
) {
    controller().refresh();
}

/// `unpair()`.
#[no_mangle]
pub extern "system" fn Java_net_grassline_app_RustBridge_unpair(
    _env: EnvUnowned<'_>,
    _class: JClass,
) {
    controller().unpair();
}

/// `dismissError()`.
#[no_mangle]
pub extern "system" fn Java_net_grassline_app_RustBridge_dismissError(
    _env: EnvUnowned<'_>,
    _class: JClass,
) {
    controller().dismiss_error();
}
