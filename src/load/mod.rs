mod parse_model;
mod parse_scene;

use anyhow::anyhow;
use bevy::{
    asset::{io::Reader, AssetLoader, AsyncReadExt, Handle, LoadContext}, log::info, pbr::StandardMaterial, render::color::Color, utils::{hashbrown::HashMap, BoxedFuture}
};
use parse_model::load_from_model;
use parse_scene::{find_model_names, find_subasset_names, parse_xform_node};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    model::{MaterialProperty, VoxelModel, VoxelPalette},
    scene::{LayerInfo, VoxelNode, VoxelScene},
};

/// An asset loader capable of loading models in `.vox` files as usable [`bevy::render::mesh::Mesh`]es.
///
/// It converts Magica Voxel's left-handed Z-up space to bevy's right-handed Y-up space.
/// The meshes generated by this asset loader only use standard [`bevy::render::mesh::Mesh`] attributes for easier compatibility with shaders.
/// You can load multiple models from the same `.vox` file by appending `#{name}` to the asset loading path, where `{name}` corresponds to the object's name in the Magical Voxel world editor.
/// You can load unnamed models by appending `#model{no}` to the asset loading path, where `{no}` corresponds to the model index in the file. Note that this index is subject to change if you delete models in the Magica Voxel file.
pub(super) struct VoxSceneLoader;

/// Settings for the VoxSceneLoader.
#[derive(Serialize, Deserialize)]
pub struct VoxLoaderSettings {
    /// Whether the outer-most faces of the model should be meshed. Defaults to true. Set this to false if the outer faces of a
    /// model will never be visible, for instance if the model id part of a 3D tileset.
    pub mesh_outer_faces: bool,
    /// Multiplier for emissive strength. Defaults to 2.0.
    pub emission_strength: f32,
    /// Defaults to `true` to more accurately reflect the colours in Magica Voxel.
    pub uses_srgb: bool,
    /// Magica Voxel doesn't let you adjust the roughness for the default "diffuse" block type, so it can be adjusted with this setting. Defaults to 0.8.
    pub diffuse_roughness: f32,
}

impl Default for VoxLoaderSettings {
    fn default() -> Self {
        Self {
            mesh_outer_faces: true,
            emission_strength: 2.0,
            uses_srgb: true,
            diffuse_roughness: 0.8,
        }
    }
}

#[derive(Error, Debug)]
pub enum VoxLoaderError {
    #[error(transparent)]
    InvalidAsset(#[from] anyhow::Error),
}

impl AssetLoader for VoxSceneLoader {
    type Asset = VoxelScene;
    type Settings = VoxLoaderSettings;
    type Error = VoxLoaderError;

    fn load<'a>(
        &'a self,
        reader: &'a mut Reader,
        _settings: &'a Self::Settings,
        load_context: &'a mut LoadContext,
    ) -> BoxedFuture<'a, Result<Self::Asset, VoxLoaderError>> {
        Box::pin(async move {
            let mut bytes = Vec::new();
            reader
                .read_to_end(&mut bytes)
                .await
                .map_err(|e| VoxLoaderError::InvalidAsset(anyhow!(e)))?;
            self.process_vox_file(&bytes, load_context, _settings)
        })
    }

    fn extensions(&self) -> &[&str] {
        &["vox"]
    }
}

impl VoxSceneLoader {
    fn process_vox_file<'a>(
        &self,
        bytes: &'a [u8],
        load_context: &'a mut LoadContext,
        settings: &'a VoxLoaderSettings,
    ) -> Result<VoxelScene, VoxLoaderError> {
        let file = match dot_vox::load_bytes(bytes) {
            Ok(data) => data,
            Err(error) => return Err(VoxLoaderError::InvalidAsset(anyhow!(error))),
        };
        info!("Loading {}", load_context.asset_path());

        // Palette
        let palette = VoxelPalette::new_from_data(
            &file,
            settings.diffuse_roughness,
            settings.emission_strength,
        );
        let translucent_material = palette.create_material_in_load_context(load_context);
        let ior_for_voxel = palette.ior_for_voxel();
        let opaque_material_handle =
            load_context.labeled_asset_scope("material".to_string(), |_| {
                let mut opaque_material = translucent_material.clone();
                opaque_material.specular_transmission_texture = None;
                opaque_material.specular_transmission = 0.0;
                opaque_material
            });
        if palette.emission == MaterialProperty::VariesPerElement {
            load_context.labeled_asset_scope("material-no-emission".to_string(), |_| {
                let mut non_emissive = translucent_material.clone();
                non_emissive.emissive_texture = None;
                non_emissive.emissive = Color::BLACK;
                non_emissive
            });
        }
        let palette_handle =
            load_context.add_labeled_asset("material-palette".to_string(), palette);
        // Scene graph

        let root = parse_xform_node(&file.scenes, &file.scenes[0], None, load_context);
        let layers: Vec<LayerInfo> = file
            .layers
            .iter()
            .map(|layer| LayerInfo {
                name: layer.name(),
                is_hidden: layer.hidden(),
            })
            .collect();
        let mut subasset_by_name: HashMap<String, VoxelNode> = HashMap::new();
        find_subasset_names(&mut subasset_by_name, &root);

        let mut model_names: Vec<Option<String>> = vec![None; file.models.len()];
        find_model_names(&mut model_names, &root);

        let models: Vec<Handle<VoxelModel>> = model_names
            .iter()
            .zip(file.models)
            .enumerate()
            .map(|(index, (maybe_name, model))| {
                let name = maybe_name.clone().unwrap_or(format!("model-{}", index));
                let data = load_from_model(&model, settings.mesh_outer_faces);
                let (visible_voxels, ior) = data.visible_voxels(&ior_for_voxel);
                let mesh = load_context.labeled_asset_scope(format!("{}@mesh", name), |_| {
                    crate::model::mesh::mesh_model(&visible_voxels, &data)
                });

                let material: Handle<StandardMaterial> = if let Some(ior) = ior {
                    load_context.labeled_asset_scope(format!("{}@material", name), |_| {
                        let mut material = translucent_material.clone();
                        material.ior = ior;
                        material.thickness =
                            model.size.x.min(model.size.y.min(model.size.z)) as f32;
                        material
                    })
                } else {
                    opaque_material_handle.clone()
                };
                load_context.labeled_asset_scope(format!("{}@model", name), |_| VoxelModel {
                    data,
                    mesh,
                    material,
                    palette: palette_handle.clone(),
                })
            })
            .collect();

        for (subscene_name, node) in subasset_by_name {
            load_context.labeled_asset_scope(subscene_name.clone(), |_| VoxelScene {
                root: node,
                layers: layers.clone(),
                models: models.clone(),
            });
        }
        Ok(VoxelScene {
            root,
            layers,
            models,
        })
    }
}
