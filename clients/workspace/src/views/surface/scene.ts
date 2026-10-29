// The 3D surface's drawing (Glass Box, results rule 5): three.js with its WebGPU renderer, which falls back to WebGL 2
// on its own; when neither works the caller shows R's still figure instead. This module loads only when a 3D view
// opens. It draws only when something moves (a drag, a key, a resize, a pick), never in a loop, so a shared machine's
// GPU is idle while a scientist reads. At most two live 3D views per window; a third gets the still.
import * as THREE from "three/webgpu";
import { OrbitControls } from "three/addons/controls/OrbitControls.js";
import { HEIGHT_SHARE } from "./angle.ts";
import { viridis, type Grid } from "./grid.ts";

let live = 0;
export const LIVE_LIMIT = 2;
export interface CameraState { position: [number, number, number]; target: [number, number, number] }
export interface SurfaceScene {
  backend: "WebGPU" | "WebGL 2";
  scale(height: number): void;
  mark(vertex: number | null): void;
  reset(): void;
  key(event: KeyboardEvent): boolean;
  camera(): CameraState;
  dispose(): void;
}

const HOME: CameraState = { position: [1.05, 0.95, 1.25], target: [0, 0.12, 0] };
/** How far the view stands back for a vertical scale, so a taller surface still fits its plate. */
const stand = (height: number) => Math.max(1, 0.75 + 0.25 * height);
/** The starting view for a plate of this shape and a vertical scale: a narrower plate (a phone) or a taller surface
 *  steps back so the whole surface shows. */
function home(aspect: number, height: number): CameraState {
  const back = Math.max(1, (4 / 3) / (aspect || 4 / 3)) * stand(height);
  const [x, y, z] = HOME.position, [tx, , tz] = HOME.target, ty = HOME.target[1] * height;
  return { position: [tx + (x - tx) * back, ty + (y - HOME.target[1]) * back, tz + (z - tz) * back], target: [tx, ty, tz] };
}

export async function createScene(host: HTMLElement, grid: Grid, options: {
  ink: string; height: number; state?: CameraState | null; onPick: (vertex: number) => void; onMove: (state: CameraState) => void;
}): Promise<SurfaceScene> {
  if (live >= LIVE_LIMIT) throw new Error("gpu_budget");
  const renderer = new THREE.WebGPURenderer({ antialias: true, alpha: true });
  await renderer.init();
  live++;
  const backend = (renderer.backend as { isWebGPUBackend?: boolean }).isWebGPUBackend ? "WebGPU" : "WebGL 2";
  renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 2));
  renderer.setClearColor(0x000000, 0);
  const canvas = renderer.domElement;
  canvas.setAttribute("aria-hidden", "true");
  host.appendChild(canvas);

  const scene = new THREE.Scene();
  const camera = new THREE.PerspectiveCamera(38, 1, 0.01, 40);
  const geometry = new THREE.BufferGeometry();
  geometry.setAttribute("position", new THREE.BufferAttribute(grid.positions, 3));
  const colors = new Float32Array(grid.levels.length * 3);
  grid.levels.forEach((level, i) => colors.set(viridis(level), i * 3));
  geometry.setAttribute("color", new THREE.BufferAttribute(colors, 3));
  geometry.setIndex(new THREE.BufferAttribute(grid.indices, 1));
  geometry.computeVertexNormals();
  const material = new THREE.MeshLambertMaterial({ vertexColors: true, side: THREE.DoubleSide });
  const surface = new THREE.Mesh(geometry, material);
  let height = options.height;
  surface.scale.y = HEIGHT_SHARE * height;
  scene.add(surface);
  // The floor of the box the values sit in, in the page's ink, so the eye keeps its bearings while it turns.
  const floor = new THREE.LineSegments(new THREE.EdgesGeometry(new THREE.PlaneGeometry(1, 1).rotateX(-Math.PI / 2)),
    new THREE.LineBasicMaterial({ color: new THREE.Color(options.ink) }));
  scene.add(floor);
  scene.add(new THREE.AmbientLight(0xffffff, 1.6));
  const sun = new THREE.DirectionalLight(0xffffff, 1.4);
  sun.position.set(0.8, 2, 1.2);
  scene.add(sun);
  const marker = new THREE.Mesh(new THREE.SphereGeometry(0.014, 16, 12), new THREE.MeshBasicMaterial({ color: new THREE.Color(options.ink) }));
  marker.visible = false;
  scene.add(marker);
  let marked: number | null = null;
  // The marker sits on the stored point at the surface's current vertical scale, without being stretched by it.
  const placeMarker = () => {
    marker.visible = marked !== null;
    if (marked === null) return;
    surface.updateMatrixWorld();
    marker.position.copy(surface.localToWorld(new THREE.Vector3().fromBufferAttribute(geometry.getAttribute("position") as THREE.BufferAttribute, marked)));
  };

  const controls = new OrbitControls(camera, canvas);
  controls.enableDamping = false;
  controls.minDistance = 0.4;
  controls.maxDistance = 6;
  const place = (state: CameraState) => {
    camera.position.set(...state.position);
    controls.target.set(...state.target);
    controls.update();
  };
  const aspectOf = () => { const box = host.getBoundingClientRect(); return box.height ? box.width / box.height : 4 / 3; };
  place(options.state ?? home(aspectOf(), height));
  // A canvas with no pixels has nothing to draw into; WebGPU refuses a zero-sized texture, so the view waits for a size.
  const draw = () => { if (canvas.width >= 1 && canvas.height >= 1 && host.isConnected) renderer.render(scene, camera); };
  const state = (): CameraState => ({ position: camera.position.toArray() as [number, number, number], target: controls.target.toArray() as [number, number, number] });
  controls.addEventListener("change", () => { draw(); options.onMove(state()); });

  const resize = () => {
    const { width, height } = host.getBoundingClientRect();
    // A host less than a pixel tall or wide (hidden, or caught mid-layout) would size the drawing buffer to zero.
    const ratio = renderer.getPixelRatio();
    if (Math.floor(width * ratio) < 1 || Math.floor(height * ratio) < 1) return;
    renderer.setSize(width, height, false);
    canvas.style.width = "100%";
    canvas.style.height = "100%";
    camera.aspect = width / height;
    camera.updateProjectionMatrix();
    draw();
  };
  const watcher = new ResizeObserver(resize);
  watcher.observe(host);
  resize();

  // A click that did not turn the view picks the stored point nearest where it landed on the surface.
  const ray = new THREE.Raycaster();
  let down: { x: number; y: number } | null = null;
  const onDown = (event: PointerEvent) => { down = { x: event.clientX, y: event.clientY }; };
  const onUp = (event: PointerEvent) => {
    if (!down || Math.hypot(event.clientX - down.x, event.clientY - down.y) > 4 || event.button !== 0) { down = null; return; }
    down = null;
    const box = canvas.getBoundingClientRect();
    ray.setFromCamera(new THREE.Vector2(((event.clientX - box.left) / box.width) * 2 - 1, -((event.clientY - box.top) / box.height) * 2 + 1), camera);
    const hit = ray.intersectObject(surface)[0];
    if (!hit?.face) return;
    const local = surface.worldToLocal(hit.point.clone());
    const at = (i: number) => new THREE.Vector3().fromBufferAttribute(geometry.getAttribute("position") as THREE.BufferAttribute, i);
    const nearest = [hit.face.a, hit.face.b, hit.face.c].sort((a, b) => at(a).distanceTo(local) - at(b).distanceTo(local))[0];
    options.onPick(nearest);
  };
  canvas.addEventListener("pointerdown", onDown);
  canvas.addEventListener("pointerup", onUp);

  // Keys turn it (arrows), zoom (plus and minus), move it (Shift with an arrow) and reset it (0).
  const turn = (left: number, up: number) => {
    const offset = camera.position.clone().sub(controls.target);
    const spherical = new THREE.Spherical().setFromVector3(offset);
    spherical.theta += left;
    spherical.phi = Math.min(Math.PI - 0.05, Math.max(0.05, spherical.phi + up));
    camera.position.copy(controls.target).add(new THREE.Vector3().setFromSpherical(spherical));
    controls.update();
  };
  const zoom = (factor: number) => {
    const offset = camera.position.clone().sub(controls.target).multiplyScalar(factor);
    if (offset.length() < controls.minDistance || offset.length() > controls.maxDistance) return;
    camera.position.copy(controls.target).add(offset);
    controls.update();
  };
  const move = (right: number, up: number) => {
    const x = new THREE.Vector3().setFromMatrixColumn(camera.matrix, 0).multiplyScalar(right);
    const y = new THREE.Vector3().setFromMatrixColumn(camera.matrix, 1).multiplyScalar(up);
    camera.position.add(x).add(y);
    controls.target.add(x).add(y);
    controls.update();
  };
  const step = Math.PI / 36;
  return {
    backend,
    // A new vertical scale keeps the reader's angle and steps back so the taller or flatter surface fits.
    scale(next) {
      const offset = camera.position.clone().sub(controls.target).multiplyScalar(stand(next) / stand(height));
      controls.target.y += HOME.target[1] * (next - height);
      height = next;
      surface.scale.y = HEIGHT_SHARE * height;
      camera.position.copy(controls.target).add(offset);
      controls.update();
      placeMarker();
      draw();
    },
    mark(vertex) { marked = vertex; placeMarker(); draw(); },
    reset() { place(home(aspectOf(), height)); },
    key(event) {
      const shift = event.shiftKey;
      switch (event.key) {
        case "ArrowLeft": shift ? move(-0.04, 0) : turn(step, 0); return true;
        case "ArrowRight": shift ? move(0.04, 0) : turn(-step, 0); return true;
        case "ArrowUp": shift ? move(0, 0.04) : turn(0, -step); return true;
        case "ArrowDown": shift ? move(0, -0.04) : turn(0, step); return true;
        case "+": case "=": zoom(0.9); return true;
        case "-": case "_": zoom(1 / 0.9); return true;
        case "0": place(home(aspectOf(), height)); return true;
        default: return false;
      }
    },
    camera: state,
    dispose() {
      watcher.disconnect();
      canvas.removeEventListener("pointerdown", onDown);
      canvas.removeEventListener("pointerup", onUp);
      controls.dispose();
      geometry.dispose(); material.dispose(); floor.geometry.dispose(); (floor.material as THREE.Material).dispose();
      marker.geometry.dispose(); (marker.material as THREE.Material).dispose();
      renderer.dispose();
      canvas.remove();
      live--;
    },
  };
}
