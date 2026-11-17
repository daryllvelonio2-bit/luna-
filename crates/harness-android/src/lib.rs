pub mod runtime;

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use jni::objects::{GlobalRef, JClass, JObject, JString, JValue};
use jni::sys::{jboolean, jint, jstring, JNI_TRUE};
use jni::{JNIEnv, JavaVM};
use runtime::{error_json, ok_json, EventSink, LunaMobileRuntime};

static RUNTIME: OnceLock<LunaMobileRuntime> = OnceLock::new();

fn get_runtime() -> Result<&'static LunaMobileRuntime, String> {
    if let Some(rt) = RUNTIME.get() {
        return Ok(rt);
    }
    let created = LunaMobileRuntime::new().map_err(|e| e.to_string())?;
    let _ = RUNTIME.set(created);
    Ok(RUNTIME.get().expect("runtime initialized"))
}

fn read_jstring(env: &mut JNIEnv, input: JString) -> Result<String, String> {
    env.get_string(&input)
        .map(|s| s.into())
        .map_err(|e| format!("failed to read jstring: {e}"))
}

fn to_jstring(env: &mut JNIEnv, text: &str) -> jstring {
    match env.new_string(text) {
        Ok(s) => s.into_raw(),
        Err(_) => env
            .new_string(r#"{"ok":false,"error":"failed to allocate JNI string"}"#)
            .expect("fallback jstring")
            .into_raw(),
    }
}

struct JniEventSink {
    jvm: JavaVM,
    callback: GlobalRef,
}

impl EventSink for JniEventSink {
    fn on_event(&self, json_payload: String) {
        let Ok(mut env) = self.jvm.attach_current_thread() else {
            return;
        };
        let Ok(arg) = env.new_string(&json_payload) else {
            return;
        };
        let _ = env.call_method(
            self.callback.as_obj(),
            "onEvent",
            "(Ljava/lang/String;)V",
            &[JValue::Object(&arg)],
        );
    }

    fn request_approval(&self, request_json: String) -> i32 {
        let Ok(mut env) = self.jvm.attach_current_thread() else {
            return 0;
        };
        let Ok(arg) = env.new_string(&request_json) else {
            return 0;
        };
        match env.call_method(
            self.callback.as_obj(),
            "requestApproval",
            "(Ljava/lang/String;)I",
            &[JValue::Object(&arg)],
        ) {
            Ok(val) => val.i().unwrap_or(0),
            Err(_) => 0,
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_ai_luna_mobile_LunaBridge_nativeInit(
    mut env: JNIEnv,
    _class: JClass,
    files_dir: JString,
    workspace_dir: JString,
) -> jstring {
    let result = (|| -> Result<String, String> {
        let files = PathBuf::from(read_jstring(&mut env, files_dir)?);
        let workspace = PathBuf::from(read_jstring(&mut env, workspace_dir)?);
        let rt = get_runtime()?;
        let init = rt.init(&files, &workspace).map_err(|e| e.to_string())?;
        Ok(ok_json(&init))
    })();
    match result {
        Ok(json) => to_jstring(&mut env, &json),
        Err(err) => to_jstring(&mut env, &error_json(err)),
    }
}

#[no_mangle]
pub extern "system" fn Java_ai_luna_mobile_LunaBridge_nativeSetApiKey(
    mut env: JNIEnv,
    _class: JClass,
    provider_id: JString,
    api_key: JString,
) -> jstring {
    let result = (|| -> Result<String, String> {
        let pid = read_jstring(&mut env, provider_id)?;
        let key = read_jstring(&mut env, api_key)?;
        let rt = get_runtime()?;
        let init = rt.set_api_key(&pid, &key).map_err(|e| e.to_string())?;
        Ok(ok_json(&init))
    })();
    match result {
        Ok(json) => to_jstring(&mut env, &json),
        Err(err) => to_jstring(&mut env, &error_json(err)),
    }
}

#[no_mangle]
pub extern "system" fn Java_ai_luna_mobile_LunaBridge_nativeListModels(
    mut env: JNIEnv,
    _class: JClass,
    provider_id: JString,
    refresh: jboolean,
) -> jstring {
    let result = (|| -> Result<String, String> {
        let pid = read_jstring(&mut env, provider_id)?;
        let rt = get_runtime()?;
        let models = rt
            .list_models(&pid, refresh == JNI_TRUE)
            .map_err(|e| e.to_string())?;
        Ok(ok_json(
            &serde_json::json!({ "ok": true, "models": models }),
        ))
    })();
    match result {
        Ok(json) => to_jstring(&mut env, &json),
        Err(err) => to_jstring(&mut env, &error_json(err)),
    }
}

#[no_mangle]
pub extern "system" fn Java_ai_luna_mobile_LunaBridge_nativeSetModel(
    mut env: JNIEnv,
    _class: JClass,
    provider_id: JString,
    model_id: JString,
) -> jstring {
    let result = (|| -> Result<String, String> {
        let pid = read_jstring(&mut env, provider_id)?;
        let mid = read_jstring(&mut env, model_id)?;
        let rt = get_runtime()?;
        let init = rt.set_model(&pid, &mid).map_err(|e| e.to_string())?;
        Ok(ok_json(&init))
    })();
    match result {
        Ok(json) => to_jstring(&mut env, &json),
        Err(err) => to_jstring(&mut env, &error_json(err)),
    }
}

#[no_mangle]
pub extern "system" fn Java_ai_luna_mobile_LunaBridge_nativeSetAutoApprove(
    mut env: JNIEnv,
    _class: JClass,
    enabled: jboolean,
) -> jstring {
    let result = (|| -> Result<String, String> {
        let rt = get_runtime()?;
        let val = rt
            .set_auto_approve(enabled == JNI_TRUE)
            .map_err(|e| e.to_string())?;
        Ok(ok_json(
            &serde_json::json!({ "ok": true, "autoApprove": val }),
        ))
    })();
    match result {
        Ok(json) => to_jstring(&mut env, &json),
        Err(err) => to_jstring(&mut env, &error_json(err)),
    }
}

#[no_mangle]
pub extern "system" fn Java_ai_luna_mobile_LunaBridge_nativeNewSession(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    let result = (|| -> Result<String, String> {
        let rt = get_runtime()?;
        let init = rt.new_session().map_err(|e| e.to_string())?;
        Ok(ok_json(&init))
    })();
    match result {
        Ok(json) => to_jstring(&mut env, &json),
        Err(err) => to_jstring(&mut env, &error_json(err)),
    }
}

#[no_mangle]
pub extern "system" fn Java_ai_luna_mobile_LunaBridge_nativeSelectSession(
    mut env: JNIEnv,
    _class: JClass,
    session_id: JString,
) -> jstring {
    let result = (|| -> Result<String, String> {
        let sid = read_jstring(&mut env, session_id)?;
        let rt = get_runtime()?;
        let init = rt.select_session(&sid).map_err(|e| e.to_string())?;
        Ok(ok_json(&init))
    })();
    match result {
        Ok(json) => to_jstring(&mut env, &json),
        Err(err) => to_jstring(&mut env, &error_json(err)),
    }
}

#[no_mangle]
pub extern "system" fn Java_ai_luna_mobile_LunaBridge_nativeDeleteSession(
    mut env: JNIEnv,
    _class: JClass,
    session_id: JString,
) -> jstring {
    let result = (|| -> Result<String, String> {
        let sid = read_jstring(&mut env, session_id)?;
        let rt = get_runtime()?;
        let init = rt.delete_session(&sid).map_err(|e| e.to_string())?;
        Ok(ok_json(&init))
    })();
    match result {
        Ok(json) => to_jstring(&mut env, &json),
        Err(err) => to_jstring(&mut env, &error_json(err)),
    }
}

#[no_mangle]
pub extern "system" fn Java_ai_luna_mobile_LunaBridge_nativeGetMessages(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    let result = (|| -> Result<String, String> {
        let rt = get_runtime()?;
        let messages = rt.get_messages(None).map_err(|e| e.to_string())?;
        Ok(ok_json(
            &serde_json::json!({ "ok": true, "messages": messages }),
        ))
    })();
    match result {
        Ok(json) => to_jstring(&mut env, &json),
        Err(err) => to_jstring(&mut env, &error_json(err)),
    }
}

#[no_mangle]
pub extern "system" fn Java_ai_luna_mobile_LunaBridge_nativeSendMessage(
    mut env: JNIEnv,
    _class: JClass,
    prompt: JString,
    callback: JObject,
) -> jstring {
    let result = (|| -> Result<String, String> {
        let prompt_str = read_jstring(&mut env, prompt)?;
        let jvm = env.get_java_vm().map_err(|e| e.to_string())?;
        let callback_ref = env.new_global_ref(callback).map_err(|e| e.to_string())?;
        let sink: Arc<dyn EventSink> = Arc::new(JniEventSink {
            jvm,
            callback: callback_ref,
        });
        let rt = get_runtime()?;
        let res = rt
            .send_message(&prompt_str, sink)
            .map_err(|e| e.to_string())?;
        Ok(ok_json(&res))
    })();
    match result {
        Ok(json) => to_jstring(&mut env, &json),
        Err(err) => to_jstring(&mut env, &error_json(err)),
    }
}

#[no_mangle]
pub extern "system" fn Java_ai_luna_mobile_LunaBridge_nativeCancelTurn(
    _env: JNIEnv,
    _class: JClass,
) -> jint {
    match get_runtime() {
        Ok(rt) => i32::from(rt.cancel_turn()),
        Err(_) => 0,
    }
}

#[no_mangle]
pub extern "system" fn Java_ai_luna_mobile_LunaBridge_nativeCompactSession(
    mut env: JNIEnv,
    _class: JClass,
    callback: JObject,
) -> jstring {
    let result = (|| -> Result<String, String> {
        let jvm = env.get_java_vm().map_err(|e| e.to_string())?;
        let callback_ref = env.new_global_ref(callback).map_err(|e| e.to_string())?;
        let sink: Arc<dyn EventSink> = Arc::new(JniEventSink {
            jvm,
            callback: callback_ref,
        });
        let rt = get_runtime()?;
        let res = rt.compact_session(sink).map_err(|e| e.to_string())?;
        Ok(ok_json(&res))
    })();
    match result {
        Ok(json) => to_jstring(&mut env, &json),
        Err(err) => to_jstring(&mut env, &error_json(err)),
    }
}

#[no_mangle]
pub extern "system" fn Java_ai_luna_mobile_LunaBridge_nativeListMemories(
    mut env: JNIEnv,
    _class: JClass,
    query: JString,
) -> jstring {
    let result = (|| -> Result<String, String> {
        let q = read_jstring(&mut env, query)?;
        let rt = get_runtime()?;
        let memories = rt.list_memories(&q).map_err(|e| e.to_string())?;
        Ok(ok_json(
            &serde_json::json!({ "ok": true, "memories": memories }),
        ))
    })();
    match result {
        Ok(json) => to_jstring(&mut env, &json),
        Err(err) => to_jstring(&mut env, &error_json(err)),
    }
}

#[no_mangle]
pub extern "system" fn Java_ai_luna_mobile_LunaBridge_nativeListNotes(
    mut env: JNIEnv,
    _class: JClass,
    query: JString,
) -> jstring {
    let result = (|| -> Result<String, String> {
        let q = read_jstring(&mut env, query)?;
        let rt = get_runtime()?;
        let notes = rt.list_notes(&q).map_err(|e| e.to_string())?;
        Ok(ok_json(&serde_json::json!({ "ok": true, "notes": notes })))
    })();
    match result {
        Ok(json) => to_jstring(&mut env, &json),
        Err(err) => to_jstring(&mut env, &error_json(err)),
    }
}
