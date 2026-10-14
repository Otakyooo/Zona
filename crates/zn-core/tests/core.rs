#[cfg(test)]
mod tests {
    use std::time::Duration;
    use zn_core::ecs::World;
    use zn_core::events::{event_count, send_event, take_events};
    use zn_core::schedule::Schedule;
    use zn_core::time::{duration_from_hz, FixedStep, SIMULATION_HZ, SIMULATION_STEP};

    #[derive(Default, Debug, PartialEq)]
    struct Counter(u32);

    #[derive(Debug, PartialEq)]
    struct Ping(u32);

    #[test]
    fn spawn_insert_query_remove() {
        let mut world = World::new();
        let entity = world.spawn((Counter(0),));
        world.insert_on(entity, Counter(7)).unwrap();
        assert_eq!(world.get::<Counter>(entity).as_deref(), Some(&Counter(7)));

        let mut query = world.query::<&mut Counter>();
        for counter in query.iter() {
            counter.0 += 1;
        }
        drop(query);
        assert_eq!(world.get::<Counter>(entity).as_deref(), Some(&Counter(8)));

        assert_eq!(world.remove::<Counter>(entity), Some(Counter(8)));
        assert!(world.get::<Counter>(entity).is_none());
        assert!(world.despawn(entity));
    }

    #[test]
    fn resources_store_typed_values() {
        let mut world = World::new();
        assert!(!world.contains_resource::<Counter>());
        world.insert_resource(Counter(3));
        assert!(world.contains_resource::<Counter>());
        assert_eq!(world.get_resource::<Counter>(), Some(&Counter(3)));
        world.get_resource_mut::<Counter>().unwrap().0 = 5;
        assert_eq!(world.remove_resource::<Counter>(), Some(Counter(5)));
    }

    #[test]
    fn schedule_runs_in_order() {
        let mut world = World::new();
        let entity = world.spawn((Counter(0),));

        let mut schedule = Schedule::new();
        schedule.add("increment", move |world: &mut World| {
            world.get_mut::<Counter>(entity).unwrap().0 += 1;
        });
        schedule.add("double", move |world: &mut World| {
            world.get_mut::<Counter>(entity).unwrap().0 *= 2;
        });
        assert_eq!(schedule.len(), 2);
        assert_eq!(
            schedule.system_names().collect::<Vec<_>>(),
            vec!["increment", "double"]
        );

        schedule.run(&mut world);
        assert_eq!(world.get::<Counter>(entity).as_deref(), Some(&Counter(2)));
    }

    #[test]
    fn fixed_step_accumulates_and_clamps() {
        let mut fixed = FixedStep::new(SIMULATION_STEP);
        assert_eq!(fixed.accumulate(SIMULATION_STEP), 1);
        assert_eq!(fixed.accumulator(), Duration::ZERO);

        let half = SIMULATION_STEP / 2;
        assert_eq!(fixed.accumulate(half), 0);
        assert_eq!(fixed.accumulate(half), 0);
        assert_eq!(fixed.accumulate(Duration::from_nanos(1)), 1);

        fixed.set_max_steps_per_frame(3);
        let steps = fixed.accumulate(SIMULATION_STEP * 10);
        assert_eq!(steps, 3);
        assert_eq!(fixed.accumulator(), Duration::ZERO);
    }

    #[test]
    fn simulation_hz_matches_step() {
        assert_eq!(duration_from_hz(SIMULATION_HZ), SIMULATION_STEP);
    }

    #[test]
    fn events_are_buffered_per_frame() {
        let mut world = World::new();
        send_event(&mut world, Ping(1));
        assert_eq!(event_count::<Ping>(&world), 1);
        assert_eq!(take_events::<Ping>(&mut world), vec![Ping(1)]);
        assert_eq!(event_count::<Ping>(&world), 0);
    }
}
