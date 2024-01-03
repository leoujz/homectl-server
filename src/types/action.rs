use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{
    dim::DimDescriptor,
    integration::CustomActionDescriptor,
    rule::ForceTriggerRoutineDescriptor,
    scene::{CycleScenesDescriptor, SceneDescriptor},
};

#[derive(TS, Clone, Deserialize, Debug, Serialize)]
#[serde(tag = "action")]
#[ts(export)]
pub enum Action {
    /// Request to activate given scene.
    ActivateScene(SceneDescriptor),

    /// Request to cycle between given scenes.
    CycleScenes(CycleScenesDescriptor),

    /// Runs a custom integration action
    Custom(CustomActionDescriptor),

    /// Dims the given groups and devices
    Dim(DimDescriptor),

    /// Forcibly triggers a routine, ignoring any possible rules
    ForceTriggerRoutine(ForceTriggerRoutineDescriptor),
}

pub type Actions = Vec<Action>;
