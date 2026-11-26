import bevy_sprite_render::mesh2d::vertex_output::VertexOutput;

@group(constants::MATERIAL_BIND_GROUP) @binding(0) var light_texture: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(1) var light_sampler: sampler;

// Карта освещения: цвет умножается на уже нарисованный мир (смешивание задаёт материал).
@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(textureSample(light_texture, light_sampler, in.uv).rgb, 1.0);
}
