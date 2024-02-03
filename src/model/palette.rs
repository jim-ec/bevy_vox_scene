use bevy::{
    asset::{Asset, Assets, Handle, LoadContext},
    pbr::StandardMaterial,
    reflect::TypePath,
    render::{
        color::Color,
        render_resource::{Extent3d, TextureDimension, TextureFormat},
        texture::Image,
    },
    utils::{default, HashMap},
};
use dot_vox::DotVoxData;

/// Container for all of the [`VoxelElement`]s that can be used in a [`super::VoxelModel`]
#[derive(Asset, TypePath, Default)]
pub struct VoxelPalette {
    pub(crate) elements: Vec<VoxelElement>,
    // material_opaque: Handle<StandardMaterial>,
    // material_translucent: Handle<StandardMaterial>,
}

/// This can be thought of as a voxel material. A type of Voxel brick modelled with physical properties such as color, roughness and so on.
pub struct VoxelElement {
    /// The base color of the voxel
    pub color: Color,
    /// The emissive strength of the voxel. This will be multiplied by the [`VoxelElement::color`] to create the emissive color
    pub emission: f32,
    /// The perceptual roughness of the voxel on a scale of 0.0 to 1.0
    pub roughness: f32,
    /// The metalness of the voxel on a scale of 0.0 to 1.0
    pub metalness: f32,
    /// The translucency or transmissiveness of the voxel on a scale of 0.0 to 1.0, with 0.0 being fully opaque and 1.0 being fully translucent
    pub translucency: f32,
    /// The index of refraction of translucent voxels. Has no effect if [`VoxelElement::translucency`] is 0.0
    pub refraction_index: f32,
}

impl Default for VoxelElement {
    fn default() -> Self {
        Self {
            color: Color::PINK,
            emission: 0.0,
            roughness: 0.5,
            metalness: 0.0,
            translucency: 0.0,
            refraction_index: 1.5,
        }
    }
}

impl VoxelPalette {
    /// Create a new [`VoxelPalette`] from the supplied [`VoxelElement`]s
    pub fn new(mut elements: Vec<VoxelElement>) -> Self {
        elements.resize_with(256, VoxelElement::default);
        VoxelPalette { elements }
    }

    /// Create a new [`VoxelPalette`] from the supplied [`Color`]s
    pub fn new_from_colors(colors: Vec<Color>) -> Self {
        VoxelPalette::new(
            colors
                .iter()
                .map(|color| VoxelElement {
                    color: *color,
                    ..default()
                })
                .collect(),
        )
    }

    pub(crate) fn new_from_data(
        data: &DotVoxData,
        diffuse_roughness: f32,
        emission_strength: f32,
    ) -> Self {
        VoxelPalette::new(
            data.palette
                .iter()
                .zip(data.materials.iter())
                .map(|(color, material)| VoxelElement {
                    color: Color::rgba_u8(color.r, color.g, color.b, color.a),
                    emission: material.emission().unwrap_or(0.0)
                        * (material.radiant_flux().unwrap_or(0.0) + 1.0)
                        * emission_strength,
                    roughness: if material.material_type() == Some("_diffuse") {
                        diffuse_roughness
                    } else {
                        material.roughness().unwrap_or(0.0)
                    },
                    metalness: material.metalness().unwrap_or(0.0),
                    translucency: material.opacity().unwrap_or(0.0),
                    refraction_index: if material.material_type() == Some("_glass") {
                        1.0 + material.refractive_index().unwrap_or(0.0)
                    } else {
                        0.0
                    },
                })
                .collect(),
        )
    }

    pub(crate) fn create_material_in_load_context(
        &self,
        load_context: &mut LoadContext,
    ) -> StandardMaterial {
        self._create_material(|name, image| load_context.add_labeled_asset(name.to_string(), image))
    }

    pub(crate) fn create_material(&self, images: &mut Assets<Image>) -> StandardMaterial {
        self._create_material(|_, image| images.add(image))
    }

    fn _create_material(
        &self,
        mut get_handle: impl FnMut(&str, Image) -> Handle<Image>,
    ) -> StandardMaterial {
        let image_size = Extent3d {
            width: 16,
            height: 16,
            depth_or_array_layers: 1,
        };
        let color_data: Vec<u8> = self
            .elements
            .iter()
            .flat_map(|e| e.color.as_rgba_u8())
            .collect();
        let emission_data: Vec<f32> = self.elements.iter().map(|e| e.emission).collect();
        let roughness_data: Vec<f32> = self.elements.iter().map(|e| e.roughness).collect();
        let metalness_data: Vec<f32> = self.elements.iter().map(|e| e.metalness).collect();
        let translucency_data: Vec<f32> = self.elements.iter().map(|e| e.translucency).collect();
        //let refraction_data: Vec<f32> = self.elements.iter().map(|e| e.refraction_index).collect();
        let max_roughness = roughness_data.max_element();
        let max_metalness = metalness_data.max_element();
        let max_translucency = translucency_data.max_element();

        let has_emission = emission_data.max_element() > 0.0;
        let has_roughness = max_roughness - roughness_data.min_element() > 0.001;
        let has_metalness = max_metalness - metalness_data.min_element() > 0.001;
        let has_roughness_metalness = has_roughness || has_metalness;
        let has_translucency = max_translucency - translucency_data.min_element() > 0.001;

        let base_color_texture = Some(get_handle(
            "material_color",
            Image::new(
                image_size,
                TextureDimension::D2,
                color_data,
                TextureFormat::Rgba8UnormSrgb,
            ),
        ));

        let emissive_texture = if has_emission {
            let emission_bytes: Vec<u8> = emission_data
                .iter()
                .zip(self.elements.iter().map(|e| e.color))
                .flat_map(|(emission, color)| {
                    (color * *emission)
                        .as_rgba_f32()
                        .iter()
                        .flat_map(|c| c.to_le_bytes())
                        .collect::<Vec<u8>>()
                })
                .collect();
            Some(get_handle(
                "material_emission",
                Image::new(
                    image_size,
                    TextureDimension::D2,
                    emission_bytes,
                    TextureFormat::Rgba32Float,
                ),
            ))
        } else {
            None
        };

        let metallic_roughness_texture: Option<Handle<Image>> = if has_roughness_metalness {
            let raw: Vec<u8> = roughness_data
                .iter()
                .zip(metalness_data.iter())
                .flat_map(|(rough, metal)| {
                    let output: Vec<u8> = [0.0, *rough, *metal, 0.0]
                        .iter()
                        .flat_map(|b| ((b * u16::MAX as f32) as u16).to_le_bytes())
                        .collect();
                    output
                })
                .collect();
            let handle = get_handle(
                "material_metallic_roughness",
                Image::new(
                    image_size,
                    TextureDimension::D2,
                    raw,
                    TextureFormat::Rgba16Unorm,
                ),
            );
            Some(handle)
        } else {
            None
        };

        let specular_transmission_texture: Option<Handle<Image>> = if has_translucency {
            let raw: Vec<u8> = translucency_data
                .iter()
                .flat_map(|t| ((t * u16::MAX as f32) as u16).to_le_bytes())
                .collect();
            let handle = get_handle(
                "material_specular_transmission",
                Image::new(
                    image_size,
                    TextureDimension::D2,
                    raw,
                    TextureFormat::R16Unorm,
                ),
            );
            Some(handle)
        } else {
            None
        };

        StandardMaterial {
            base_color_texture,
            emissive: if has_emission {
                Color::WHITE
            } else {
                Color::BLACK
            },
            emissive_texture,
            perceptual_roughness: if has_roughness_metalness {
                1.0
            } else {
                max_roughness
            },
            metallic: if has_roughness_metalness {
                1.0
            } else {
                max_metalness
            },
            metallic_roughness_texture,
            specular_transmission: if has_translucency {
                1.0
            } else {
                max_translucency
            },
            specular_transmission_texture,
            ..default()
        }
    }

    pub(crate) fn ior_for_voxel(&self) -> HashMap<u8, f32> {
        let mut result = HashMap::new();
        for (index, element) in self.elements.iter().enumerate() {
            if element.translucency > 0.0 {
                result.insert(index as u8, element.refraction_index);
            }
        }
        result
    }
}

trait VecComparable<T> {
    fn max_element(&self) -> T;

    fn min_element(&self) -> T;
}

impl VecComparable<f32> for Vec<f32> {
    fn max_element(&self) -> f32 {
        self.iter()
            .cloned()
            .max_by(|a, b| a.partial_cmp(b).expect("tried to compare NaN"))
            .unwrap()
    }

    fn min_element(&self) -> f32 {
        self.iter()
            .cloned()
            .min_by(|a, b| a.partial_cmp(b).expect("tried to compare NaN"))
            .unwrap()
    }
}
