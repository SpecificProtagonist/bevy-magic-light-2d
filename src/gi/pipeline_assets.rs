use bevy::prelude::*;
use bevy::render::render_resource::{StorageBuffer, UniformBuffer};
use bevy::render::renderer::{RenderDevice, RenderQueue};
use bevy::render::Extract;
use rand::{thread_rng, Rng};

use crate::gi::constants::GI_SCREEN_PROBE_SIZE;
use crate::gi::resource::ComputedTargetSizes;
use crate::gi::types::{LightOccluder2D, OmniLightSource2D, SkylightLight2D, SkylightMask2D};
use crate::gi::types_gpu::{
    GpuCameraParams, GpuLightOccluder2D, GpuLightOccluderBuffer, GpuLightPassParams,
    GpuLightSourceBuffer, GpuOmniLightSource, GpuProbeDataBuffer, GpuSkylightMaskBuffer,
    GpuSkylightMaskData,
};
use crate::prelude::LightPassParams;
use crate::MainCamera;

#[rustfmt::skip]
#[derive(Default, Resource)]
pub(crate) struct LightPassPipelineAssets {
    pub camera_params:     UniformBuffer<GpuCameraParams>,
    pub light_pass_params: UniformBuffer<GpuLightPassParams>,
    pub light_sources:     StorageBuffer<GpuLightSourceBuffer>,
    pub light_occluders:   StorageBuffer<GpuLightOccluderBuffer>,
    pub probes:            StorageBuffer<GpuProbeDataBuffer>,
    pub skylight_masks:    StorageBuffer<GpuSkylightMaskBuffer>,
}

impl LightPassPipelineAssets {
    pub fn write_buffer(&mut self, device: &RenderDevice, queue: &RenderQueue) {
        self.light_sources.write_buffer(device, queue);
        self.light_occluders.write_buffer(device, queue);
        self.camera_params.write_buffer(device, queue);
        self.light_pass_params.write_buffer(device, queue);
        self.probes.write_buffer(device, queue);
        self.skylight_masks.write_buffer(device, queue);
    }
}

#[rustfmt::skip]
pub(crate) fn system_prepare_pipeline_assets(
    render_device:         Res<RenderDevice>,
    render_queue:          Res<RenderQueue>,
    mut gi_compute_assets: ResMut<LightPassPipelineAssets>,
) {
    gi_compute_assets.write_buffer(&render_device, &render_queue);
}

#[rustfmt::skip]
pub(crate) fn system_extract_pipeline_assets(
    res_light_pass_config:      Extract<Res<LightPassParams>>,
    res_target_sizes:           Extract<Res<ComputedTargetSizes>>,

    query_lights:               Extract<Query<(&Transform, &OmniLightSource2D, &ComputedVisibility)>>,
    query_occluders:            Extract<Query<(&LightOccluder2D, &Transform, &ComputedVisibility)>>,
    query_camera:               Extract<Query<(&Camera, &GlobalTransform), With<MainCamera>>>,
    query_masks:                Extract<Query<(&Transform, &SkylightMask2D)>>,
    query_skylight_light:       Extract<Query<&SkylightLight2D>>,

    mut gpu_target_sizes:       ResMut<ComputedTargetSizes>,
    mut gpu_pipeline_assets:    ResMut<LightPassPipelineAssets>,
    mut gpu_frame_counter:      Local<i32>,
) {

    *gpu_target_sizes = **res_target_sizes;

    {
        let mut light_sources = gpu_pipeline_assets.light_sources.get_mut();
        let mut rng = thread_rng();
        light_sources.count = 0;
        light_sources.data.clear();
        for (transform, light_source, visibility) in query_lights.iter() {
            if visibility.is_visible() {
                light_sources.count += 1;
                light_sources.data.push(GpuOmniLightSource::new(
                    OmniLightSource2D {
                        intensity: light_source.intensity
                            + rng.gen_range(-1.0..1.0) * light_source.jitter_intensity,
                        ..*light_source
                    },
                    Vec2::new(
                        transform.translation.x
                            + rng.gen_range(-1.0..1.0) * light_source.jitter_translation,
                        transform.translation.y
                            + rng.gen_range(-1.0..1.0) * light_source.jitter_translation,
                    ),
                ));
            }
        }
    }

    {
        let mut light_occluders = gpu_pipeline_assets.light_occluders.get_mut();
        light_occluders.count = 0;
        light_occluders.data.clear();
        for (occluder, transform, visibility) in query_occluders.iter() {
            if visibility.is_visible() {
                light_occluders.count += 1;
                light_occluders.data.push(GpuLightOccluder2D::new(
                    transform.translation.truncate(),
                    occluder.h_size,
                ));
            }
        }
    }

    {
        let mut skylight_masks = gpu_pipeline_assets.skylight_masks.get_mut();
        skylight_masks.count = 0;
        skylight_masks.data.clear();
        for (transform, mask) in query_masks.iter() {
            skylight_masks.count += 1;
            skylight_masks.data.push(GpuSkylightMaskData::new(
                transform.translation.truncate(),
                mask.h_size,
            ));
        }
    }

    {
        if let Ok((camera, camera_global_transform)) = query_camera.get_single() {
            let mut camera_params = gpu_pipeline_assets.camera_params.get_mut();
            let projection = camera.projection_matrix();
            let inverse_projection = projection.inverse();
            let view = camera_global_transform.compute_matrix();
            let inverse_view = view.inverse();

            camera_params.view_proj = projection * inverse_view;
            camera_params.inverse_view_proj = view * inverse_projection;
            camera_params.screen_size = Vec2::new(
                gpu_target_sizes.primary_target_size.x,
                gpu_target_sizes.primary_target_size.y,
            );
            camera_params.screen_size_inv = Vec2::new(
                1.0 / gpu_target_sizes.primary_target_size.x,
                1.0 / gpu_target_sizes.primary_target_size.y,
            );

            let scale = 2.0;
            camera_params.sdf_scale = Vec2::splat(scale);
            camera_params.inv_sdf_scale = Vec2::splat(1. / scale);

            let probes = gpu_pipeline_assets.probes.get_mut();
            probes.data[*gpu_frame_counter as usize].camera_pose =
                camera_global_transform.translation().truncate();
        } else {
            let probes = gpu_pipeline_assets.probes.get_mut();
            probes.data[*gpu_frame_counter as usize].camera_pose = Vec2::ZERO;
        }
    }

    {
        let cols = gpu_target_sizes.primary_target_isize.x as i32 / GI_SCREEN_PROBE_SIZE;
        let rows = gpu_target_sizes.primary_target_isize.y as i32 / GI_SCREEN_PROBE_SIZE;

        let mut light_pass_params = gpu_pipeline_assets.light_pass_params.get_mut();
        light_pass_params.frame_counter = *gpu_frame_counter;
        light_pass_params.probe_size = GI_SCREEN_PROBE_SIZE;
        light_pass_params.probe_atlas_cols = cols;
        light_pass_params.probe_atlas_rows = rows;
        light_pass_params.reservoir_size = res_light_pass_config.reservoir_size;
        light_pass_params.smooth_kernel_size_h = res_light_pass_config.smooth_kernel_size.0;
        light_pass_params.smooth_kernel_size_w = res_light_pass_config.smooth_kernel_size.1;
        light_pass_params.direct_light_contrib = res_light_pass_config.direct_light_contrib;
        light_pass_params.indirect_light_contrib = res_light_pass_config.indirect_light_contrib;
    }

    {
        let mut light_pass_params = gpu_pipeline_assets.light_pass_params.get_mut();
        light_pass_params.skylight_color = Vec3::splat(0.0);
        for new_gi_state in query_skylight_light.iter() {
            light_pass_params.skylight_color.x += new_gi_state.color.r() * new_gi_state.intensity;
            light_pass_params.skylight_color.y += new_gi_state.color.g() * new_gi_state.intensity;
            light_pass_params.skylight_color.z += new_gi_state.color.b() * new_gi_state.intensity;
        }
    }

    *gpu_frame_counter = (*gpu_frame_counter + 1) % (GI_SCREEN_PROBE_SIZE * GI_SCREEN_PROBE_SIZE);
}