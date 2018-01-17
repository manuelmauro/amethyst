//! Simple shaded pass

use std::marker::PhantomData;
use std::mem;

use amethyst_assets::AssetStorage;
use amethyst_core::cgmath::{Matrix4, One, SquareMatrix};
use amethyst_core::transform::Transform;
use gfx::pso::buffer::ElemStride;
use specs::{Fetch, Join, ReadStorage};

use super::*;
use cam::{ActiveCamera, Camera};
use error::Result;
use light::{DirectionalLight, Light, PointLight};
use mesh::{Mesh, MeshHandle};
use mtl::{Material, MaterialDefaults};
use pipe::{DepthMode, Effect, NewEffect};
use pipe::pass::{Pass, PassData};
use resources::AmbientColor;
use tex::Texture;
use types::{Encoder, Factory};
use vertex::{Normal, Position, Query, TexCoord};

/// Draw mesh with simple lighting technique
/// `V` is `VertexFormat`
#[derive(Derivative, Clone, Debug, PartialEq)]
#[derivative(Default(bound = "V: Query<(Position, Normal, TexCoord)>"))]
pub struct DrawShaded<V> {
    _pd: PhantomData<V>,
}

impl<V> DrawShaded<V>
where
    V: Query<(Position, Normal, TexCoord)>,
{
    /// Create instance of `DrawShaded` pass
    pub fn new() -> Self {
        Default::default()
    }
}

impl<'a, V> PassData<'a> for DrawShaded<V>
where
    V: Query<(Position, Normal, TexCoord)>,
{
    type Data = (
        Option<Fetch<'a, ActiveCamera>>,
        ReadStorage<'a, Camera>,
        Fetch<'a, AmbientColor>,
        Fetch<'a, AssetStorage<Mesh>>,
        Fetch<'a, AssetStorage<Texture>>,
        Fetch<'a, MaterialDefaults>,
        ReadStorage<'a, MeshHandle>,
        ReadStorage<'a, Material>,
        ReadStorage<'a, Transform>,
        ReadStorage<'a, Light>,
    );
}

impl<V> Pass for DrawShaded<V>
where
    V: Query<(Position, Normal, TexCoord)>,
{
    fn compile(&self, effect: NewEffect) -> Result<Effect> {
        effect
            .simple(VERT_SRC, FRAG_SRC)
            .with_raw_vertex_buffer(V::QUERIED_ATTRIBUTES, V::size() as ElemStride, 0)
            .with_raw_constant_buffer("VertexArgs", mem::size_of::<VertexArgs>(), 1)
            .with_raw_constant_buffer("FragmentArgs", mem::size_of::<FragmentArgs>(), 1)
            .with_raw_constant_buffer("PointLights", mem::size_of::<PointLight>(), 128)
            .with_raw_constant_buffer("DirectionalLights", mem::size_of::<DirectionalLight>(), 16)
            .with_raw_global("ambient_color")
            .with_raw_global("camera_position")
            .with_texture("emission")
            .with_texture("albedo")
            .with_output("out_color", Some(DepthMode::LessEqualWrite))
            .build()
    }

    fn apply<'a, 'b: 'a>(
        &'a mut self,
        encoder: &mut Encoder,
        effect: &mut Effect,
        _factory: Factory,
        (
            active,
            camera,
            ambient,
            mesh_storage,
            tex_storage,
            material_defaults,
            mesh,
            material,
            global,
            light,
        ): <Self as PassData<'a>>::Data,
    ) {
        let camera: Option<(&Camera, &Transform)> = active
            .and_then(|a| {
                let cam = camera.get(a.entity);
                let transform = global.get(a.entity);
                cam.into_iter().zip(transform.into_iter()).next()
            })
            .or_else(|| (&camera, &global).join().next());
        effect.update_global("ambient_color", Into::<[f32; 3]>::into(*ambient.as_ref()));
        effect.update_global(
            "camera_position",
            camera
                .as_ref()
                .map(|&(_, ref trans)| [trans.0[3][0], trans.0[3][1], trans.0[3][2]])
                .unwrap_or([0.0; 3]),
        );

        let point_lights: Vec<PointLightPod> = light
            .join()
            .filter_map(|light| {
                if let Light::Point(ref light) = *light {
                    Some(PointLightPod {
                        position: pad(light.center.into()),
                        color: pad(light.color.into()),
                        intensity: light.intensity,
                        _pad: [0.0; 3],
                    })
                } else {
                    None
                }
            })
            .collect();

        let directional_lights: Vec<DirectionalLightPod> = light
            .join()
            .filter_map(|light| {
                if let Light::Directional(ref light) = *light {
                    Some(DirectionalLightPod {
                        color: pad(light.color.into()),
                        direction: pad(light.direction.into()),
                    })
                } else {
                    None
                }
            })
            .collect();

        let fragment_args = FragmentArgs {
            point_light_count: point_lights.len() as i32,
            directional_light_count: directional_lights.len() as i32,
        };

        effect.update_constant_buffer("FragmentArgs", &fragment_args, encoder);
        effect.update_buffer("PointLights", &point_lights[..], encoder);
        effect.update_buffer("DirectionalLights", &directional_lights[..], encoder);
        for (mesh, material, global) in (&mesh, &material, &global).join() {
            let mesh = match mesh_storage.get(mesh) {
                Some(mesh) => mesh,
                None => continue,
            };
            let vbuf = match mesh.buffer(V::QUERIED_ATTRIBUTES) {
                Some(vbuf) => vbuf.clone(),
                None => continue,
            };

            let vertex_args = camera
                .as_ref()
                .map(|&(ref cam, ref transform)| VertexArgs {
                    proj: cam.proj.into(),
                    view: transform.0.invert().unwrap().into(),
                    model: *global.as_ref(),
                })
                .unwrap_or_else(|| VertexArgs {
                    proj: Matrix4::one().into(),
                    view: Matrix4::one().into(),
                    model: *global.as_ref(),
                });

            effect.update_constant_buffer("VertexArgs", &vertex_args, encoder);

            let albedo = tex_storage
                .get(&material.albedo)
                .or_else(|| tex_storage.get(&material_defaults.0.albedo))
                .unwrap();

            let emission = tex_storage
                .get(&material.emission)
                .or_else(|| tex_storage.get(&material_defaults.0.emission))
                .unwrap();

            effect.data.textures.push(emission.view().clone());

            effect.data.samplers.push(emission.sampler().clone());

            effect.data.textures.push(albedo.view().clone());
            effect.data.samplers.push(albedo.sampler().clone());

            effect.data.vertex_bufs.push(vbuf);

            effect.draw(mesh.slice(), encoder);
            effect.clear();
        }
    }
}
