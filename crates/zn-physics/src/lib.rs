use rapier3d::control::{EffectiveCharacterMovement, KinematicCharacterController};
use rapier3d::pipeline::{PhysicsPipeline, QueryFilter};
use rapier3d::prelude::*;

pub const PLAYER_RADIUS: f32 = 0.3;
pub const PLAYER_HALF_HEIGHT: f32 = 0.6;
pub const PLAYER_TOTAL_HEIGHT: f32 = 2.0 * (PLAYER_HALF_HEIGHT + PLAYER_RADIUS);
pub const PLAYER_EYE_HEIGHT: f32 = 1.62;

#[derive(Clone, Copy, Debug)]
pub struct CharacterMove {
    pub translation: Vector,
    pub grounded: bool,
    pub sliding_down_slope: bool,
}

impl From<EffectiveCharacterMovement> for CharacterMove {
    fn from(movement: EffectiveCharacterMovement) -> Self {
        Self {
            translation: movement.translation,
            grounded: movement.grounded,
            sliding_down_slope: movement.is_sliding_down_slope,
        }
    }
}

pub struct PhysicsWorld {
    gravity: Vector,
    integration_parameters: IntegrationParameters,
    pipeline: PhysicsPipeline,
    bodies: RigidBodySet,
    colliders: ColliderSet,
    islands: IslandManager,
    broad_phase: BroadPhaseBvh,
    narrow_phase: NarrowPhase,
    impulse_joints: ImpulseJointSet,
    multibody_joints: MultibodyJointSet,
    soft_bodies: SoftBodySet,
    ccd_solver: CCDSolver,
    controller: KinematicCharacterController,
    character_shape: Collider,
    static_colliders: usize,
}

impl PhysicsWorld {
    pub fn new(gravity: Vector, fixed_dt: f32) -> Self {
        let mut integration_parameters = IntegrationParameters::default();
        integration_parameters.dt = fixed_dt;

        let mut controller = KinematicCharacterController::default();
        controller.slide = true;
        controller.max_slope_climb_angle = (45.0f32).to_radians();
        controller.min_slope_slide_angle = (30.0f32).to_radians();
        controller.normal_nudge_factor = 0.002;

        let character_shape = ColliderBuilder::capsule_y(PLAYER_HALF_HEIGHT, PLAYER_RADIUS).build();

        Self {
            gravity,
            integration_parameters,
            pipeline: PhysicsPipeline::new(),
            bodies: RigidBodySet::new(),
            colliders: ColliderSet::new(),
            islands: IslandManager::new(),
            broad_phase: BroadPhaseBvh::new(),
            narrow_phase: NarrowPhase::new(),
            impulse_joints: ImpulseJointSet::new(),
            multibody_joints: MultibodyJointSet::new(),
            soft_bodies: SoftBodySet::new(),
            ccd_solver: CCDSolver::new(),
            controller,
            character_shape,
            static_colliders: 0,
        }
    }

    pub fn with_gravity_y(gravity_y: f32, fixed_dt: f32) -> Self {
        Self::new(Vector::new(0.0, gravity_y, 0.0), fixed_dt)
    }

    pub fn add_static_box(&mut self, half_extents: Vector, position: Vector) -> ColliderHandle {
        self.add_static_box_with_friction(half_extents, position, 0.8)
    }

    pub fn add_static_box_with_friction(
        &mut self,
        half_extents: Vector,
        position: Vector,
        friction: f32,
    ) -> ColliderHandle {
        let collider = ColliderBuilder::cuboid(half_extents.x, half_extents.y, half_extents.z)
            .translation(position)
            .friction(friction)
            .build();
        let handle = self.colliders.insert(collider);
        self.static_colliders += 1;
        handle
    }

    pub fn static_collider_count(&self) -> usize {
        self.static_colliders
    }

    pub fn colliders(&self) -> &ColliderSet {
        &self.colliders
    }

    pub fn colliders_mut(&mut self) -> &mut ColliderSet {
        &mut self.colliders
    }

    pub fn fixed_dt(&self) -> f32 {
        self.integration_parameters.dt
    }

    pub fn gravity(&self) -> Vector {
        self.gravity
    }

    pub fn step_dynamic_bodies(&mut self) {
        let Self {
            pipeline,
            gravity,
            integration_parameters,
            bodies,
            colliders,
            islands,
            broad_phase,
            narrow_phase,
            impulse_joints,
            multibody_joints,
            soft_bodies,
            ccd_solver,
            ..
        } = self;

        pipeline.step(
            *gravity,
            integration_parameters,
            islands,
            broad_phase,
            narrow_phase,
            bodies,
            colliders,
            impulse_joints,
            multibody_joints,
            soft_bodies,
            ccd_solver,
            &(),
            &(),
        );
    }

    pub fn query_pipeline(&self) -> QueryPipeline<'_> {
        self.broad_phase.as_query_pipeline(
            self.narrow_phase.query_dispatcher(),
            &self.bodies,
            &self.colliders,
            QueryFilter::default(),
        )
    }

    pub fn cast_ray_down(&self, origin: Vector, max_distance: f32) -> bool {
        let ray = Ray::new(origin, Vector::new(0.0, -1.0, 0.0));
        self.query_pipeline().cast_ray(&ray, max_distance, true).is_some()
    }

    pub fn move_character(&mut self, origin: Vector, desired_translation: Vector) -> CharacterMove {
        let Self {
            integration_parameters,
            controller,
            character_shape,
            bodies,
            colliders,
            broad_phase,
            narrow_phase,
            ..
        } = self;

        let query_pipeline = broad_phase.as_query_pipeline(
            narrow_phase.query_dispatcher(),
            bodies,
            colliders,
            QueryFilter::default(),
        );
        let pose = Pose::from_translation(origin);

        controller
            .move_shape(
                integration_parameters.dt,
                &query_pipeline,
                character_shape.shape(),
                &pose,
                desired_translation,
                |_| {},
            )
            .into()
    }

    pub fn character_origin(feet_position: Vector) -> Vector {
        feet_position + Vector::new(0.0, PLAYER_HALF_HEIGHT + PLAYER_RADIUS, 0.0)
    }

    pub fn character_feet(origin: Vector) -> Vector {
        origin - Vector::new(0.0, PLAYER_HALF_HEIGHT + PLAYER_RADIUS, 0.0)
    }
}

impl Default for PhysicsWorld {
    fn default() -> Self {
        Self::new(Vector::new(0.0, -9.81, 0.0), 1.0 / 60.0)
    }
}
