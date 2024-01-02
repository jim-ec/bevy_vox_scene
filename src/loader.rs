
use anyhow::{anyhow, Context};
use bevy::{
    asset::{io::Reader, AssetLoader, AsyncReadExt, LoadContext, Handle},
    render::{mesh::Mesh, texture::Image, render_resource::{Extent3d, TextureDimension, TextureFormat}, color::Color},
    utils::BoxedFuture, pbr::{StandardMaterial, AlphaMode},
};
use serde::{Deserialize, Serialize};
use block_mesh::QuadCoordinateConfig;
use dot_vox::SceneNode;
use thiserror::Error;

/// An asset loader capable of loading models in `.vox` files as usable [`bevy::render::mesh::Mesh`]es.
///
/// The meshes generated by this asset loader only use standard [`bevy::render::mesh::Mesh`] attributes for easier compatibility with shaders.
/// You can load multiple models from the same `.vox` file by appending `#{name}` to the asset loading path, where `{name}` corresponds to the object's name in the Magical Voxel world editor.
/// You can load unnamed models by appending `#model{no}` to the asset loading path, where `{no}` corresponds to the model index in the file. Note that this index is subject to change if you delete models in the Magica Voxel file.
pub struct VoxLoader {
    /// Whether to flip the UVs vertically when meshing the models.
    /// You may want to change this to false if you aren't using Vulkan as a graphical backend for bevy , else this should default to true.
    pub(crate) config: QuadCoordinateConfig,
    pub(crate) v_flip_face: bool,
}

#[derive(Serialize, Deserialize)]
pub struct VoxLoaderSettings {
    pub emission_strength: f32,
}

impl Default for VoxLoaderSettings {
    fn default() -> Self {
        Self { emission_strength: 2.0 }
    }
}

#[derive(Error, Debug)]
pub enum VoxLoaderError {
    #[error(transparent)]
    InvalidAsset(#[from] anyhow::Error),
}

impl AssetLoader for VoxLoader {
    type Asset = Mesh;
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
            Ok(self.process_vox_file(&bytes, load_context, _settings)?)
        })
    }
    
    fn extensions(&self) -> &[&str] {
        &["vox"]
    }
}

impl VoxLoader {
    fn process_vox_file<'a>(
        &self,
        bytes: &'a [u8],
        load_context: &'a mut LoadContext,
        settings: &'a VoxLoaderSettings,
    ) -> Result<Mesh, VoxLoaderError> {
        let file = match dot_vox::load_bytes(bytes) {
            Ok(data) => data,
            Err(error) => return Err(VoxLoaderError::InvalidAsset(anyhow!(error))),
        };
        
        // Color
        let color_data: Vec<u8> = file.palette.iter().zip(file.materials.iter()).flat_map(|(c, m)| {
            let mut rgba: [u8; 4] = c.into(); 
            if let Some(opacity) = m.opacity() {
                rgba[3] = ((1.0 - opacity) * u8::MAX as f32) as u8;
            }
            rgba
        }).collect();
        let color_image = Image::new(Extent3d { width: 256, height: 1, depth_or_array_layers: 1 }, TextureDimension::D2, color_data, TextureFormat::Rgba8Unorm);
        let color_handle = load_context.add_labeled_asset("material_base_color".to_string(), color_image);
        
        // Emissive
        let emissive_data: Vec<Option<f32>> = file.materials.iter().map(|m| {
            if let Some(emission) = m.emission() {
                if let Some(radiance) = m.radiant_flux() {
                    Some(emission * (radiance + 1.0))
                } else {
                    Some(emission)
                }
            } else {
                None
            }
        }).collect();
        let has_emissive = !emissive_data.iter().flatten().cloned().collect::<Vec<f32>>().is_empty();
        let emissive_texture: Option<Handle<Image>> = if has_emissive {
            let emissive_raw: Vec<u8> = emissive_data.iter().zip(file.palette.iter()).flat_map(|(emission, color)| {
                if let Some(value) = emission {
                    let rgba: [u8; 4] = color.into();
                    let output: Vec<u8> = rgba.iter().flat_map(|b| ((*b as f32 / u8::MAX as f32) * value).to_le_bytes() ).collect();
                    output
                } else {
                    let rgba: [f32; 4] = [0.0, 0.0, 0.0, 0.0];
                    let output: Vec<u8> = rgba.iter().flat_map(|b| b.to_le_bytes()).collect();
                    output
                }
            }).collect();
            let emissive_image = Image::new(Extent3d { width: 256, height: 1, depth_or_array_layers: 1 }, TextureDimension::D2, emissive_raw, TextureFormat::Rgba32Float);
            let emissive_handle = load_context.add_labeled_asset("material_emission".to_string(), emissive_image);
            Some(emissive_handle)
        } else {
            None
        };
        
        // Roughness/ Metalness
        let roughness: Vec<f32> = file.materials.iter().map(|m| {
            m.roughness().unwrap_or(0.0)
        }).collect();
        let max_roughness = roughness.iter().cloned().max_by(|a, b| a.partial_cmp(b).expect("tried to compare NaN")).unwrap();
        let has_varying_roughness = max_roughness - roughness.iter().cloned().min_by(|a, b| a.partial_cmp(b).expect("tried to compare NaN")).unwrap() > 0.0;
        
        let metalness: Vec<f32> = file.materials.iter().map(|m| {
            m.metalness().unwrap_or(0.0)
        }).collect();
        let max_metalness = metalness.iter().cloned().max_by(|a, b| a.partial_cmp(b).expect("tried to compare NaN")).unwrap();
        let has_varying_metalness = max_metalness - metalness.iter().cloned().min_by(|a, b| a.partial_cmp(b).expect("tried to compare NaN")).unwrap() > 0.0;
        let has_metallic_roughness = has_varying_roughness || has_varying_metalness;
        let metallic_roughness_texture: Option<Handle<Image>> = if has_metallic_roughness {
            let raw: Vec<u8> = roughness.iter().zip(metalness.iter()).flat_map(|(rough, metal)| {
                let output: Vec<u8> = [0.0, *rough, *metal, 0.0].iter().flat_map(|b| b.to_le_bytes()).collect();
                output
            }).collect();
            let image = Image::new(Extent3d { width: 256, height: 1, depth_or_array_layers: 1 }, TextureDimension::D2, raw, TextureFormat::Rgba32Float);
            let handle = load_context.add_labeled_asset("material_metallic_roughness".to_string(), image);
            Some(handle)
        } else {
            None
        };
        
        // Specular transmission
        let transparency_data: Vec<Option<f32>> = file.materials.iter().map(|m| m.opacity()).collect();
        let has_transparency = !transparency_data.iter().flatten().cloned().collect::<Vec<f32>>().is_empty();
        let specular_transmission_texture: Option<Handle<Image>> = if has_transparency {
            let raw: Vec<u8> = transparency_data.iter().flat_map(|t| {
                t.unwrap_or(0.0).to_le_bytes()
            }).collect();
            let image = Image::new(Extent3d { width: 256, height: 1, depth_or_array_layers: 1 }, TextureDimension::D2, raw, TextureFormat::R32Float);
            let handle = load_context.add_labeled_asset("material_specular_transmission".to_string(), image);
            Some(handle)
        } else {
            None
        };
        let iors: Vec<f32> = file.materials.iter().map(|m| m.refractive_index() ).flatten().collect();
        let average_ior = if iors.len() > 0 { 1.0 + (iors.iter().cloned().reduce(|acc, e| acc + e).unwrap_or(0.0) / iors.len() as f32) } else { 0.0 };
        let translucent_voxel_indices: Vec<u8> = file.materials.iter().enumerate().filter_map(|(i, val)| if val.opacity().is_some() { Some(i as u8) } else { None }).collect();

        // Material
        let material = StandardMaterial {
            base_color_texture: Some(color_handle),
            emissive: if has_emissive { Color::WHITE * settings.emission_strength } else { Color::BLACK },
            emissive_texture,
            perceptual_roughness: if has_metallic_roughness { 1.0 } else { max_roughness },
            metallic: if has_metallic_roughness { 1.0 } else { max_metalness },
            metallic_roughness_texture,
            specular_transmission: if has_transparency { 1.0 } else { 0.0 },
            specular_transmission_texture: specular_transmission_texture,
            ior: average_ior,
            alpha_mode: if has_transparency { AlphaMode::Blend } else { AlphaMode::Opaque },
            thickness: if has_transparency { 4.0 } else { 0.0 },
            ..Default::default()
        };
        load_context.add_labeled_asset("material".to_string(), material);
        
        // Models
        let named_models = parse_scene_graph(&file.scenes, &file.scenes[0], &None);
        let mut default_mesh: Option<Mesh> = None;
        for NamedModel { name, id } in named_models {
            let Some(model) = file.models.get(id as usize) else { continue };
            let (shape, buffer) = crate::voxel::load_from_model(model, &translucent_voxel_indices);
            let mesh =
            crate::mesh::mesh_model(shape, &buffer,  &self.config);
            if id == 0 {
                default_mesh = Some(mesh.clone());
            }
            load_context.add_labeled_asset(name, mesh);
        }           
        Ok(default_mesh.context("No models found in vox file")?)
    }
}

struct NamedModel {
    name: String,
    id: u32,
}

fn parse_scene_graph(
    graph: &Vec<SceneNode>,
    node: &SceneNode,
    node_name: &Option<String>,
) -> Vec<NamedModel> {
    match node {
        SceneNode::Transform { attributes, frames: _, child, layer_id: _ } => {
            let handle: Option<String> = match (node_name, &attributes.get("_name")) {
                (None, None) => None,
                (None, Some(name)) => Some(name.to_string()),
                (Some(name), None) => Some(name.to_string()),
                (Some(a), Some(b)) => Some(format!("{a}-{b}")),
            };
            parse_scene_graph(graph, &graph[*child as usize], &handle)
        }
        SceneNode::Group { attributes: _, children } => {
            children.iter().flat_map(|child| {
                parse_scene_graph(graph, &graph[*child as usize], node_name)
            }).collect()
        }
        SceneNode::Shape { attributes: _, models } => {
            models.iter().map(|model| {
                let handle = if let Some(name) = node_name { name.to_owned() } else { format!("model{}", model.model_id) };
                NamedModel { name: handle, id: model.model_id }
            }).collect()
        }
    }
}