use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use cached::proc_macro::cached;
use evalexpr::*;
use eyre::Result;
use jsonptr::Assign;
use serde_json_path::JsonPath;

use crate::types::{
    action::Action,
    device::DevicesState,
    event::{Message, TxEventChannel},
    group::{FlattenedGroupsConfig, GroupId},
    integration::{CustomActionDescriptor, IntegrationActionPayload, IntegrationId},
    rule::{ForceTriggerRoutineDescriptor, RoutineId},
    scene::{FlattenedScenesConfig, SceneDescriptor, SceneId},
};

use super::{
    groups::{flattened_groups_to_eval_context_values, Groups},
    scenes::Scenes,
};

fn value_kv_pairs_deep(
    value: &serde_json::Value,
    prefix: String,
) -> Vec<(String, serde_json::Value)> {
    match value {
        serde_json::Value::Object(object) => object
            .iter()
            .flat_map(|(key, value)| {
                let key = format!("{}.{}", prefix, key);
                value_kv_pairs_deep(value, key)
            })
            .collect(),
        serde_json::Value::Array(array) => array
            .iter()
            .enumerate()
            .flat_map(|(i, value)| {
                let key = format!("{}.{}", prefix, i);
                value_kv_pairs_deep(value, key)
            })
            .collect(),
        _ => vec![(prefix, value.clone())],
    }
}

fn serde_value_to_evalexpr(value: &serde_json::Value) -> Result<Value> {
    match value {
        serde_json::Value::Bool(b) => Ok(Value::Boolean(*b)),
        serde_json::Value::Number(n) => {
            Ok(Value::Float(n.as_f64().ok_or_else(|| {
                eyre!("Failed to convert serde number to evalexpr float")
            })?))
        }
        serde_json::Value::String(s) => Ok(Value::String(s.clone())),
        serde_json::Value::Null => Ok(Value::Empty),
        serde_json::Value::Array(_) => Err(eyre!("Arrays are not supported for rule evaluation")),
        serde_json::Value::Object(_) => Err(eyre!("Objects are not supported for rule evaluation")),
    }
}

fn evalexpr_value_to_serde(value: &Value) -> Result<serde_json::Value> {
    match value {
        Value::Boolean(b) => Ok(serde_json::Value::Bool(*b)),
        Value::Float(f) => Ok(serde_json::Value::Number(
            serde_json::Number::from_f64(*f)
                .ok_or_else(|| eyre!("Failed to convert evalexpr float to serde number"))?,
        )),
        Value::String(s) => Ok(serde_json::Value::String(s.clone())),
        Value::Empty => Ok(serde_json::Value::Null),
        Value::Tuple(a) => Ok(serde_json::Value::Array(
            a.iter()
                .map(evalexpr_value_to_serde)
                .collect::<Result<Vec<_>>>()?,
        )),
        Value::Int(i) => Ok(serde_json::Value::Number(serde_json::Number::from(*i))),
    }
}

fn name_to_evalexpr(device_name: &str) -> String {
    device_name.to_lowercase().replace(' ', "_")
}

#[cached(size = 1, result = true)]
pub fn state_to_eval_context(
    devices: DevicesState,
    flattened_scenes: FlattenedScenesConfig,
    flattened_groups: FlattenedGroupsConfig,
) -> Result<HashMapContext> {
    let mut context = HashMapContext::new();

    for device in devices.0.values() {
        let root_value = device.get_value();
        let prefix = format!(
            "devices.{}.{}",
            device.integration_id,
            name_to_evalexpr(&device.name)
        );
        let values = value_kv_pairs_deep(&root_value, prefix);

        for (key, value) in values {
            let value = serde_value_to_evalexpr(&value).unwrap();
            context.set_value(key, value)?;
        }
    }

    for (scene_id, scene) in flattened_scenes.0 {
        let prefix = format!("scenes.{}", name_to_evalexpr(&scene_id.to_string()));

        for (device_key, state) in scene.devices.0 {
            let device = devices.0.get(&device_key);

            let Some(device) = device else {
                continue;
            };

            let integration_id = &device.integration_id;
            let name = name_to_evalexpr(&device.name.to_lowercase());
            let prefix = format!("{prefix}.{integration_id}.{name}");

            let value = serde_json::to_value(state)?;
            let values = value_kv_pairs_deep(&value, prefix.clone());

            for (key, value) in values {
                let value = serde_value_to_evalexpr(&value)?;
                context.set_value(key, value)?;
            }
        }
    }

    let group_eval_context_values =
        flattened_groups_to_eval_context_values(flattened_groups, devices);

    for (key, value) in group_eval_context_values {
        let value = serde_value_to_evalexpr(&value)?;
        context.set_value(key, value)?;
    }

    context.set_function("dbg".into(), {
        let context = context.clone();

        Function::new(move |argument| {
            if argument.is_empty() {
                debug_print_context(&context)
            } else {
                dbg!(&argument);
            }
            Ok(Value::Empty)
        })
    })?;

    Ok(context)
}

fn tuple_value_to_vec_string(value: &Value) -> EvalexprResult<Vec<String>> {
    let tuple = value.as_tuple()?;
    let vec: Vec<String> = tuple
        .into_iter()
        .map(|k| k.as_string())
        .collect::<EvalexprResult<Vec<_>>>()?;

    Ok(vec)
}

pub fn eval_action_expr(
    expr: &Node,
    devices: DevicesState,
    scenes: Scenes,
    groups: Groups,
    event_tx: &TxEventChannel,
) -> Result<()> {
    let flattened_scenes = scenes.get_flattened_scenes(&devices);
    let flattened_groups = groups.get_flattened_groups(&devices);
    let mut context = state_to_eval_context(devices.clone(), flattened_scenes, flattened_groups)?;
    context.set_type_safety_checks_disabled(true)?;
    let original_context = context.clone();
    let actions = Arc::new(RwLock::new(Vec::<EvalExprAction>::new()));

    #[derive(Clone)]
    enum EvalExprAction {
        ActivateScene(SceneId),
        Custom(IntegrationId, IntegrationActionPayload),
        ForceTriggerRoutine(RoutineId),
    }

    {
        let actions = actions.clone();
        context.set_function(
            "activate_scene".into(),
            Function::new(move |argument| {
                let scene_id = argument.as_string()?.into();
                actions
                    .write()
                    .unwrap()
                    .push(EvalExprAction::ActivateScene(scene_id));
                Ok(Value::Empty)
            }),
        )?;
    }

    {
        let actions = actions.clone();
        context.set_function(
            "custom_action".into(),
            Function::new(move |argument| {
                let arguments = argument.as_tuple()?;
                let integration_id = arguments[0].as_string()?.into();
                let payload = tuple_value_to_vec_string(&arguments[1])?.join("").into();
                actions
                    .write()
                    .unwrap()
                    .push(EvalExprAction::Custom(integration_id, payload));
                Ok(Value::Empty)
            }),
        )?;
    }

    {
        let actions = actions.clone();
        context.set_function(
            "trigger_routine".into(),
            Function::new(move |argument| {
                let arguments = argument.as_tuple()?;
                let routine_id = arguments[0].as_string()?.into();
                actions
                    .write()
                    .unwrap()
                    .push(EvalExprAction::ForceTriggerRoutine(routine_id));
                Ok(Value::Empty)
            }),
        )?;
    }

    let result = expr.eval_with_context_mut(&mut context)?;

    // Skip actions dispatch if expression evaluated to false
    if let Value::Boolean(false) = result {
        return Ok(());
    }

    for action in actions.read().unwrap().iter() {
        let action = match action.clone() {
            EvalExprAction::ActivateScene(scene_id) => {
                let group_keys = context.get_value("group_keys").map_or(Ok(None), |v| {
                    let group_ids = tuple_value_to_vec_string(v)
                        .map(|vec| vec.into_iter().map(GroupId).collect());

                    Some(group_ids).transpose()
                })?;

                Action::ActivateScene(SceneDescriptor {
                    scene_id,
                    device_keys: None,
                    group_keys,
                })
            }
            EvalExprAction::Custom(integration_id, payload) => {
                Action::Custom(CustomActionDescriptor {
                    integration_id,
                    payload,
                })
            }
            EvalExprAction::ForceTriggerRoutine(routine_id) => {
                Action::ForceTriggerRoutine(ForceTriggerRoutineDescriptor { routine_id })
            }
        };

        event_tx.send(Message::Action(action));
    }

    let vars_diff: HashMap<String, Value> = context
        .iter_variables()
        .filter_map(|(name, value)| {
            let original_value = original_context.get_value(&name);
            if Some(&value) != original_value {
                Some((name.clone(), value.clone()))
            } else {
                None
            }
        })
        .collect();

    debug!("The expression changed the value of the following variables:");
    debug!("{vars_diff:?}");

    let mut vars_diff_map = serde_json::Value::default();

    for (path, value) in vars_diff {
        let json_pointer = jsonptr::Pointer::try_from(format!("/{}", path.replace('.', "/")))?;
        let new_value = evalexpr_value_to_serde(&value)?;
        vars_diff_map.assign(&json_pointer, new_value)?;
    }

    let scenes_path = JsonPath::parse("$.devices.*.*.scene").unwrap();
    let state_path = JsonPath::parse("$.devices.*.*.state").unwrap();

    let find_device_by_path = |path: &Vec<String>| {
        let integration_id = path.get(1).unwrap();
        let name = path.get(2).unwrap();

        devices.0.values().find(|device| {
            &device.integration_id.to_string() == integration_id
                && &name_to_evalexpr(&device.name) == name
        })
    };

    let scenes_diff = scenes_path.query_path_and_value(&vars_diff_map);
    let state_diff = state_path.query_path_and_value(&vars_diff_map);

    for (path, scene_id) in scenes_diff {
        let Some(device) = find_device_by_path(&path) else {
            continue;
        };

        let scene_id = scene_id.as_str().map(|s| SceneId::new(s.to_string()));

        if let Some(scene_id) = scene_id {
            event_tx.send(Message::Action(Action::ActivateScene(SceneDescriptor {
                scene_id,
                device_keys: Some(vec![device.get_device_key()]),
                group_keys: None,
            })));
        }
    }

    for (path, state) in state_diff {
        let Some(device) = find_device_by_path(&path) else {
            continue;
        };

        let device = device.set_value(state);
        if let Ok(device) = device {
            event_tx.send(Message::Action(Action::SetDeviceState(device)));
        }
    }

    Ok(())
}

pub fn debug_print_context(context: &HashMapContext) {
    let mut vars_sorted = context
        .iter_variables()
        .map(|(name, value)| format!("{name} = {value}"))
        .collect::<Vec<_>>();
    vars_sorted.sort();

    dbg!(&vars_sorted);
}
