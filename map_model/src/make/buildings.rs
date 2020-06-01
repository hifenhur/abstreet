use crate::make::sidewalk_finder::find_sidewalk_points;
use crate::raw::{OriginalBuilding, RawBuilding, RawParkingLot};
use crate::{
    osm, Building, BuildingID, FrontPath, LaneID, LaneType, Map, OffstreetParking, ParkingLot,
    ParkingLotID, Position,
};
use abstutil::Timer;
use geom::{Distance, HashablePt2D, Line, PolyLine, Polygon};
use std::collections::{BTreeMap, HashSet};

pub fn make_all_buildings(
    input: &BTreeMap<OriginalBuilding, RawBuilding>,
    map: &Map,
    timer: &mut Timer,
) -> Vec<Building> {
    timer.start("convert buildings");
    let mut center_per_bldg: BTreeMap<OriginalBuilding, HashablePt2D> = BTreeMap::new();
    let mut query: HashSet<HashablePt2D> = HashSet::new();
    timer.start_iter("get building center points", input.len());
    for (id, b) in input {
        timer.next();
        let center = b.polygon.center().to_hashable();
        center_per_bldg.insert(*id, center);
        query.insert(center);
    }

    // Skip buildings that're too far away from their sidewalk
    let sidewalk_pts = find_sidewalk_points(
        map.get_bounds(),
        query,
        map.all_lanes(),
        Distance::meters(100.0),
        timer,
    );

    let mut results = Vec::new();
    timer.start_iter("create building front paths", center_per_bldg.len());
    for (orig_id, bldg_center) in center_per_bldg {
        timer.next();
        if let Some(sidewalk_pos) = sidewalk_pts.get(&bldg_center) {
            let sidewalk_pt = sidewalk_pos.pt(map);
            if sidewalk_pt == bldg_center.to_pt2d() {
                timer.warn(format!(
                    "Skipping building {} because front path has 0 length",
                    orig_id
                ));
                continue;
            }
            let b = &input[&orig_id];
            let sidewalk_line =
                trim_path(&b.polygon, Line::new(bldg_center.to_pt2d(), sidewalk_pt));

            let id = BuildingID(results.len());
            let mut bldg = Building {
                id,
                polygon: b.polygon.clone(),
                address: get_address(&b.osm_tags, sidewalk_pos.lane(), map),
                name: b.osm_tags.get(osm::NAME).cloned(),
                osm_way_id: orig_id.osm_way_id,
                front_path: FrontPath {
                    sidewalk: *sidewalk_pos,
                    line: sidewalk_line.clone(),
                },
                amenities: b.amenities.clone(),
                parking: None,
                label_center: b.polygon.polylabel(),
            };

            // Can this building have a driveway? If it's not next to a driving lane, then no.
            let sidewalk_lane = sidewalk_pos.lane();
            if let Ok(driving_lane) = map
                .get_parent(sidewalk_lane)
                .find_closest_lane(sidewalk_lane, vec![LaneType::Driving])
            {
                let driving_pos = sidewalk_pos.equiv_pos(driving_lane, Distance::ZERO, map);

                let buffer = Distance::meters(7.0);
                if driving_pos.dist_along() > buffer
                    && map.get_l(driving_lane).length() - driving_pos.dist_along() > buffer
                {
                    let driveway_line = PolyLine::new(vec![
                        sidewalk_line.pt1(),
                        sidewalk_line.pt2(),
                        driving_pos.pt(map),
                    ]);
                    bldg.parking = Some(OffstreetParking {
                        public_garage_name: b.public_garage_name.clone(),
                        num_spots: b.num_parking_spots,
                        driveway_line,
                        driving_pos,
                    });
                }
            }
            if bldg.parking.is_none() {
                timer.warn(format!(
                    "{} can't have a driveway. Forfeiting {} parking spots",
                    bldg.id, b.num_parking_spots
                ));
            }

            results.push(bldg);
        }
    }

    timer.note(format!(
        "Discarded {} buildings that weren't close enough to a sidewalk",
        input.len() - results.len()
    ));
    timer.stop("convert buildings");

    results
}

pub fn make_all_parking_lots(
    input: &Vec<RawParkingLot>,
    map: &Map,
    timer: &mut Timer,
) -> Vec<ParkingLot> {
    timer.start("convert parking lots");
    let mut center_per_lot: Vec<HashablePt2D> = Vec::new();
    let mut query: HashSet<HashablePt2D> = HashSet::new();
    for lot in input {
        let center = lot.polygon.center().to_hashable();
        center_per_lot.push(center);
        query.insert(center);
    }

    let sidewalk_pts = find_sidewalk_points(
        map.get_bounds(),
        query,
        map.all_lanes(),
        Distance::meters(500.0),
        timer,
    );

    let mut results = Vec::new();
    timer.start_iter("create parking lot driveways", center_per_lot.len());
    for (lot_center, orig) in center_per_lot.into_iter().zip(input.iter()) {
        timer.next();
        // TODO Refactor this
        if let Some(sidewalk_pos) = sidewalk_pts.get(&lot_center) {
            let sidewalk_pt = sidewalk_pos.pt(map);
            if sidewalk_pt == lot_center.to_pt2d() {
                timer.warn(format!(
                    "Skipping parking lot {} because driveway has 0 length",
                    orig.osm_id
                ));
                continue;
            }
            let sidewalk_line =
                trim_path(&orig.polygon, Line::new(lot_center.to_pt2d(), sidewalk_pt));

            // Can this lot have a driveway? If it's not next to a driving lane, then no.
            let mut driveway: Option<(PolyLine, Position)> = None;
            let sidewalk_lane = sidewalk_pos.lane();
            if let Ok(driving_lane) = map
                .get_parent(sidewalk_lane)
                .find_closest_lane(sidewalk_lane, vec![LaneType::Driving])
            {
                let driving_pos = sidewalk_pos.equiv_pos(driving_lane, Distance::ZERO, map);

                let buffer = Distance::meters(7.0);
                if driving_pos.dist_along() > buffer
                    && map.get_l(driving_lane).length() - driving_pos.dist_along() > buffer
                {
                    driveway = Some((
                        PolyLine::new(vec![
                            sidewalk_line.pt1(),
                            sidewalk_line.pt2(),
                            driving_pos.pt(map),
                        ]),
                        driving_pos,
                    ));
                }
            }
            if let Some((driveway_line, driving_pos)) = driveway {
                let id = ParkingLotID(results.len());
                results.push(ParkingLot {
                    id,
                    polygon: orig.polygon.clone(),
                    // TODO Rethink this approach. 250 square feet is around 23 square meters
                    capacity: orig
                        .capacity
                        .unwrap_or_else(|| (orig.polygon.area() / 23.0) as usize),
                    osm_id: orig.osm_id,

                    driveway_line,
                    driving_pos,
                    sidewalk_line,
                    sidewalk_pos: *sidewalk_pos,
                });
            } else {
                timer.warn(format!(
                    "Parking lot from OSM way {} can't have a driveway. Forfeiting {:?} parking \
                     spots",
                    orig.osm_id, orig.capacity
                ));
            }
        }
    }

    timer.note(format!(
        "Discarded {} parking lots that weren't close enough to a sidewalk",
        input.len() - results.len()
    ));
    timer.stop("convert parking lots");

    results
}

// Adjust the path to start on the building's border, not center
fn trim_path(poly: &Polygon, path: Line) -> Line {
    for bldg_line in poly.points().windows(2) {
        let l = Line::new(bldg_line[0], bldg_line[1]);
        if let Some(hit) = l.intersection(&path) {
            if let Some(l) = Line::maybe_new(hit, path.pt2()) {
                return l;
            }
        }
    }
    // Just give up
    path
}

fn get_address(tags: &BTreeMap<String, String>, sidewalk: LaneID, map: &Map) -> String {
    match (tags.get("addr:housenumber"), tags.get("addr:street")) {
        (Some(num), Some(st)) => format!("{} {}", num, st),
        (None, Some(st)) => format!("??? {}", st),
        _ => format!("??? {}", map.get_parent(sidewalk).get_name()),
    }
}
