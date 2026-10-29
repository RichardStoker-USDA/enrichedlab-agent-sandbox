// The 3D view's angle as base R's persp() takes it (Glass Box, results rule 4: the 3D view can send its angle and
// vertical scale to R for a new figure version). persp() looks from the front, the lowest y, at theta 0 and from the
// highest x at theta 90 (checked against R 4.6 drawings); phi is the height of the eye above the floor
// in degrees; expand scales the heights after persp() has made x, y and z the same length, as the 3D view does before
// its vertical scale. The scene keeps the table's x along three.js x, its y along -z and its heights along +y, so the
// two are the same right-handed space.
import type { CameraState } from "./scene.ts";

/** The heights fill this share of the width at vertical scale 1, so the surface reads at a glance; the view prints its
 *  scale, and persp()'s expand is this share times the scale. */
export const HEIGHT_SHARE = 0.35;
export interface PerspAngle { theta: number; phi: number; expand: number }
const round = (value: number, places: number) => Number(value.toFixed(places));

export function perspAngle(camera: CameraState, height: number): PerspAngle {
  const [px, py, pz] = camera.position, [tx, ty, tz] = camera.target;
  const dx = px - tx, dy = py - ty, dz = pz - tz;
  const theta = (Math.atan2(dx, dz) * 180) / Math.PI;
  const phi = (Math.atan2(dy, Math.hypot(dx, dz)) * 180) / Math.PI;
  return { theta: round(theta, 1), phi: round(Math.min(90, Math.max(0, phi)), 1), expand: round(HEIGHT_SHARE * height, 3) };
}
