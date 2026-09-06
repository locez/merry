mod form;
mod manager;
mod model_picker;

pub(crate) use super::provider_selection::ModelPickerTarget;
pub(crate) use form::{
    ProviderDraftMode, ProviderFormField, ProviderFormOverlay, ProviderFormSeed, ProviderFormValues,
};
pub(crate) use manager::{ProviderListItem, ProviderManagerOverlay};
use merry_llm::ReasoningEffort;
pub(crate) use model_picker::{ModelListItem, ModelPickerOverlay};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProviderOverlayAction {
    Consumed,
    Back,
    OpenProviderManager,
    OpenProviderForm,
    OpenProviderEditor(String),
    OpenModelPicker(String),
    BackToProviderForm,
    DiscoverFormModels {
        original_alias: Option<String>,
        values: ProviderFormValues,
    },
    SaveProvider(ProviderFormValues),
    UpdateProvider {
        original_alias: String,
        values: ProviderFormValues,
    },
    RefreshModels(String),
    RefreshFormModels,
    DeleteProvider(String),
    SelectProvider(String),
    SelectModel {
        alias: String,
        model: String,
        target: ModelPickerTarget,
    },
    SelectReasoning {
        alias: String,
        model: String,
        reasoning_effort: ReasoningEffort,
        target: ModelPickerTarget,
    },
}

#[cfg(test)]
mod tests;
