//! Camera input (local and geodetic), the floating origin and the coordinate conversions.

use super::*;

impl World {

    /// Camera from geodetic input (contract `update_camera_geodetic`): `lat`/`lon` WGS84
    /// degrees, `alt_msl_m` meters above mean sea level, `vel_ned` m/s (north, east, down).
    /// Converted to the local frame through the current origin (AGL = altitude - terrain,
    /// velocity x = E, y = -D, z = -N), then the same path as [`World::update_camera`],
    /// re-centering included. Invalid input (non-finite, |lat| > 90, |lon| > 180) is logged
    /// and ignored (no state change); returns `Err` with the reason then.
    pub fn update_camera_geodetic(
        &mut self,
        lat: f64,
        lon: f64,
        alt_msl_m: f64,
        vel_ned: [f64; 3],
    ) -> Result<Option<[f64; 3]>, String> {
        if self.scheduler.is_none() {
            return Ok(None);
        }
        let checked = check_geodetic(lat, lon, alt_msl_m).and_then(|()| {
            if vel_ned.iter().all(|v| v.is_finite()) {
                Ok(())
            } else {
                Err(format!("vel_ned = {vel_ned:?} is not finite"))
            }
        });
        if let Err(e) = checked {
            let msg = format!("WorldStreamer: update_camera_geodetic ignored: {e}");
            self.warn(&msg);
            return Err(e);
        }
        let Some(p) = self.geodetic_to_local(lat, lon, alt_msl_m) else { return Ok(None) };
        let agl = alt_msl_m - self.terrain.height(lat, lon);
        let vel = [vel_ned[1], -vel_ned[2], -vel_ned[0]];
        Ok(self.update_camera(p, vel, agl))
    }

    /// WGS84 point (degrees, meters above MSL) -> current local frame (x east, y = altitude
    /// above MSL, z -north), through the current floating origin. `None` before configure,
    /// after `shutdown` and for invalid input.
    pub fn geodetic_to_local(&self, lat: f64, lon: f64, alt_msl_m: f64) -> Option<[f64; 3]> {
        let f = self.frame.as_ref()?;
        check_geodetic(lat, lon, alt_msl_m).ok()?;
        let [e, n] = f.enu(lat, lon);
        let [x, z] = self.local.to_local(e, n);
        Some([x, alt_msl_m, z])
    }

    /// Current local frame point -> `[lat, lon, alt_msl]` (degrees, meters). Inverse of
    /// [`World::geodetic_to_local`]. `None` before configure, after `shutdown` and for
    /// non-finite input.
    pub fn local_to_geodetic(&self, p: [f64; 3]) -> Option<[f64; 3]> {
        let f = self.frame.as_ref()?;
        if !p.iter().all(|v| v.is_finite()) {
            return None;
        }
        let [e, n] = self.local.to_enu(p[0], p[2]);
        let (lat, lon) = f.lat_lon(e, n);
        Some([lat, lon, p[1]])
    }

    /// Call every frame. `local_pos` and `velocity` are in the local frame (x east, y up,
    /// z -north). Re-centers the floating origin when needed, recomputes the wanted set and
    /// queues jobs. Returns the origin shift applied by this call (also reported once by
    /// `take_origin_shift`, which this does not consume), `None` if there was none.
    pub fn update_camera(&mut self, local_pos: [f64; 3], velocity: [f64; 3], alt_agl_m: f64) -> Option<[f64; 3]> {
        self.scheduler.as_ref()?;
        let timer = self.call_timer();
        self.redelivered_this_frame = 0;
        self.drain_worker();
        let t_drained = Instant::now();
        let (x, z) = (local_pos[0], local_pos[2]);
        let cam_en = self.local.to_enu(x, z);
        let alt = if alt_agl_m.is_finite() { alt_agl_m.max(0.0) } else { 0.0 };
        let cam = [cam_en[0], cam_en[1], alt];
        // Local velocity (x east, y up, z -north) -> (east, north, up).
        let vel = [velocity[0], -velocity[2], velocity[1]];

        // Floating origin: translate (never rotate) the local frame onto the camera.
        let mut shift = None;
        if crate::math::hypot(x, z) > self.profile.origin.recenter_distance_m {
            self.local.offset_e += x;
            self.local.offset_n -= z;
            self.pending_shift[0] += x;
            self.pending_shift[2] += z;
            shift = Some([x, 0.0, z]);
        }

        let now = Instant::now();
        // `fades.max_active` counts the swaps of both layers.
        if let Some(s) = &mut self.scheduler {
            s.set_fades_elsewhere(self.terrain_sched.as_ref().map_or(0, |t| t.active_fades()));
            s.update(cam, vel, now, &mut self.jobs);
            s.sync_memory(&mut self.memory);
        }
        if let Some(t) = &mut self.terrain_sched {
            t.set_fades_elsewhere(self.scheduler.as_ref().map_or(0, |s| s.active_fades()));
            t.update(cam, vel, now, &mut self.jobs);
        }
        let t_updated = Instant::now();
        self.cancel_orphans();
        self.order_jobs();
        self.submit_jobs();
        self.submit_probes();
        if let Some(t) = timer {
            let end = Instant::now();
            self.end_call_with("update_camera", timer, || {
                let (a, b) = (t.us_until(t_drained), t.us_until(t_updated));
                format!("(drain {a}us, scheduler {}us, submit {}us)", b - a, t.us_until(end) - b)
            });
            if end.duration_since(self.latency_logged) >= LATENCY_LOG_EVERY {
                self.latency_logged = end;
                if let Some(l) = self.latency() {
                    self.log.info(&format!("WorldStreamer profile: {}", l.report()));
                }
                if let Some(p) = self.poll_profile.take_summary() {
                    self.log.info(&format!("WorldStreamer profile: {p}"));
                }
                if let Some(t) = &self.terrain_sched {
                    self.log.info(&format!("WorldStreamer profile: {}", t.cpu_stats().report("terrain", t.layer().rings())));
                }
            }
        }
        shift
    }

    /// Non-zero once after a re-center (local x/y/z meters); the host subtracts it from the
    /// camera and all tile roots.
    pub fn take_origin_shift(&mut self) -> [f64; 3] {
        std::mem::replace(&mut self.pending_shift, [0.0; 3])
    }
}

/// Geodetic input check: finite, |lat| <= 90, |lon| <= 180.
fn check_geodetic(lat: f64, lon: f64, alt: f64) -> Result<(), String> {
    if !(lat.is_finite() && lat.abs() <= 90.0) {
        return Err(format!("lat = {lat} is out of range [-90, 90]"));
    }
    if !(lon.is_finite() && lon.abs() <= 180.0) {
        return Err(format!("lon = {lon} is out of range [-180, 180]"));
    }
    if !alt.is_finite() {
        return Err(format!("alt_msl_m = {alt} is not finite"));
    }
    Ok(())
}
