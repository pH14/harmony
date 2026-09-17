// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;

use blue_workload::{
    advice::{AdviceContext, BlueAdviser, map_name},
    progress::MILESTONE_NAMES,
};

fn voted_goal(adviser: &BlueAdviser, context: AdviceContext) -> (Option<u8>, Vec<(u8, f64)>) {
    let mut total: BTreeMap<u8, f64> = BTreeMap::new();
    for _ in 0..3 {
        let (shares, _) = adviser
            .goal_shares(context)
            .expect("Jev answers the target map question");
        for (map, share) in shares {
            *total.entry(map).or_default() += share;
        }
    }
    let named = blue_workload::campaign::decided(&total);
    let mut ranked = total.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| right.1.total_cmp(&left.1));
    ranked.truncate(3);
    (named, ranked)
}

#[test]
#[ignore = "needs TYPESAFE_API_KEY and reaches the Jev API"]
fn jev_holds_a_majority_for_the_map_every_milestone_but_the_parcel_pickup_lands_on() {
    let adviser = BlueAdviser::from_environment().expect("TYPESAFE_API_KEY names a Jev key");
    let expected = [
        (0b0_u8, Some(40_u8)),
        (0b1, None),
        (0b11, Some(40)),
        (0b111, Some(40)),
        (0b1111, Some(51)),
        (0b1_1111, Some(2)),
        (0b11_1111, Some(54)),
        (0b111_1111, Some(54)),
    ];
    assert_eq!(MILESTONE_NAMES.len(), expected.len());
    let mut wrong = Vec::new();
    for (route, map) in expected {
        let context = AdviceContext {
            badges: 0,
            route,
            map: 0,
        };
        let (named, ranked) = voted_goal(&adviser, context);
        println!(
            "next={} named={:?} expected={:?} top={:?}",
            context.next_milestone(),
            named.map(map_name),
            map.map(map_name),
            ranked
                .iter()
                .map(|(map, share)| (map_name(*map), (share * 100.0).round() / 100.0))
                .collect::<Vec<_>>(),
        );
        if named != map {
            wrong.push((context.next_milestone(), named.map(map_name)));
        }
    }
    assert!(wrong.is_empty(), "Jev named the wrong map: {wrong:?}");
}
