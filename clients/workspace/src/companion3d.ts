// The figure companion's 3D scene. It is imported only for a retained request with
// three mapped numeric columns. WebGPURenderer uses WebGPU when available and its
// maintained WebGL 2 backend otherwise.
import * as THREE from "three/webgpu";
import { OrbitControls } from "three/addons/controls/OrbitControls.js";
import { attribute, instancedDynamicBufferAttribute, mix, pointUV, texture as textureNode, uniform, vec4 } from "three/tsl";
import {
  columnFaceLight,
  companionLayout,
  fitCompanionCamera,
  paletteColour,
  type CompanionLayout,
  type CompanionMathPoint,
} from "./companion3d_math.ts";
import { nearestVisibleInstanceRow, nearestVisiblePointRow } from "./companion3d_pick.ts";

export interface Companion3DPoint extends CompanionMathPoint {}

export interface Companion3DOptions {
  surface: boolean;
  palette: string[];
  theme(): { ink: string; grid: string };
  reducedMotion: boolean;
  onHover(row: number | null): void;
  onPick(row: number): void;
}

export interface Companion3DScene {
  backend: "WebGPU" | "WebGL 2";
  mode: "surface" | "points";
  fallback: CompanionLayout["fallback"];
  key(event: KeyboardEvent): boolean;
  zoom(direction: "in" | "out"): void;
  reset(): void;
  setVisible(groups: Set<string>): void;
  setActive(row: number | null): void;
  setSelected(rows: Set<number>): void;
  setBrushMode(enabled: boolean): void;
  /** Select marks whose stored-row centers project inside host-local CSS pixels. */
  brush(rect: { left: number; top: number; right: number; bottom: number }): number[];
  dispose(): void;
}

let live = 0;
const LIVE_LIMIT = 2;
const clamp = (value: number, low = 0, high = 1) => Math.min(high, Math.max(low, value));

function markerTexture(ring: boolean): THREE.DataTexture {
  const size = 64;
  const data = new Uint8Array(size * size * 4);
  const edge = 2 / size;
  for (let y = 0; y < size; y++) for (let x = 0; x < size; x++) {
    const at = (y * size + x) * 4;
    const dx = (x + 0.5) / size - 0.5, dy = (y + 0.5) / size - 0.5;
    const distance = Math.hypot(dx, dy);
    const outside = clamp((0.5 - distance) / edge);
    const inside = ring ? clamp((distance - 0.32) / edge) : 1;
    data[at] = 255; data[at + 1] = 255; data[at + 2] = 255;
    data[at + 3] = Math.round(255 * outside * inside);
  }
  const texture = new THREE.DataTexture(data, size, size, THREE.RGBAFormat, THREE.UnsignedByteType);
  texture.minFilter = THREE.LinearFilter;
  texture.magFilter = THREE.LinearFilter;
  texture.generateMipmaps = false;
  texture.needsUpdate = true;
  return texture;
}

function storedColour(stops: string[], level: number): THREE.Color {
  const [r, g, b] = paletteColour(stops, level);
  return new THREE.Color().setRGB(r, g, b, THREE.SRGBColorSpace);
}

function tickLayer(host: HTMLElement, layout: CompanionLayout) {
  const layer = document.createElement("div");
  layer.className = "companion-axis-ticks";
  layer.setAttribute("aria-hidden", "true");
  host.appendChild(layer);
  const labels: {
    axis: "x" | "y" | "z";
    at: number;
    element: HTMLSpanElement;
    position: THREE.Vector3;
    outside: THREE.Vector3;
  }[] = [];
  const add = (axis: "x" | "y" | "z", at: number, text: string) => {
    const element = document.createElement("span");
    element.className = `companion-axis-tick companion-axis-${axis}`;
    element.textContent = text;
    layer.appendChild(element);
    labels.push({ axis, at, element, position: new THREE.Vector3(), outside: new THREE.Vector3() });
  };
  for (const tick of layout.ticks.x) add("x", tick.at, tick.text);
  for (const tick of layout.ticks.y) add("y", tick.at, tick.text);
  for (const tick of layout.ticks.z) add("z", tick.at, tick.text);
  return { layer, labels };
}

export async function createCompanion3D(
  host: HTMLElement,
  points: Companion3DPoint[],
  options: Companion3DOptions,
): Promise<Companion3DScene> {
  if (live >= LIVE_LIMIT) throw new Error("gpu_budget");
  if (!points.length) throw new Error("no_points");
  const renderer = new THREE.WebGPURenderer({ antialias: true, alpha: true });
  await renderer.init();
  live++;
  let disposed = false;
  const backend = (renderer.backend as { isWebGPUBackend?: boolean }).isWebGPUBackend ? "WebGPU" : "WebGL 2";
  renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 2));
  renderer.setClearColor(0x000000, 0);
  const canvas = renderer.domElement;
  canvas.setAttribute("aria-hidden", "true");
  host.appendChild(canvas);

  const layout = companionLayout(points, options.surface);
  const scene = new THREE.Scene();
  const camera = new THREE.PerspectiveCamera(40, 1, 0.01, 50);
  const controls = new OrbitControls(camera, canvas);
  controls.enableDamping = false;
  controls.enablePan = false;
  controls.autoRotate = false;
  // Every path is render-on-demand and has no camera tween, including reduced motion.
  if (options.reducedMotion) controls.rotateSpeed = 0.75;

  const positionOf = new Map<number, THREE.Vector3>();
  const groupOf = new Map<number, string>();
  for (const mark of layout.marks) {
    positionOf.set(mark.row, new THREE.Vector3(...mark.position));
    groupOf.set(mark.row, mark.group);
  }
  const markRows = layout.marks.map((mark) => mark.row);
  const markGroups = layout.marks.map((mark) => mark.group);
  let visibleGroups = new Set(markGroups);

  const disc = markerTexture(false);
  const ring = markerTexture(true);
  const pointInk = new THREE.Color(options.theme().ink);
  const materials: THREE.Material[] = [];
  let columnGeometry: THREE.BoxGeometry | null = null;
  let columnMesh: THREE.InstancedMesh | null = null;
  let pointGeometry: THREE.BufferGeometry | null = null;
  let pointObject: THREE.Points | null = null;
  let pointColours: Float32Array | null = null;
  let pointColourAttribute: THREE.BufferAttribute | null = null;
  const columnMatrix = new THREE.Matrix4();
  const columnPosition = new THREE.Vector3(), columnScale = new THREE.Vector3();
  const columnRotation = new THREE.Quaternion();
  const placeColumn = (index: number) => {
    const mark = layout.marks[index], column = mark.column!;
    columnPosition.set(mark.position[0], column.base, mark.position[2]);
    if (visibleGroups.has(mark.group)) {
      // A minimum-valued column has mathematically zero height. A tiny GPU-only scale keeps
      // its top face pickable without inventing a visible height or changing the stored mark.
      columnScale.set(column.width, Math.max(column.height, 1e-5), column.depth);
    } else columnScale.set(0, 0, 0);
    columnMatrix.compose(columnPosition, columnRotation, columnScale);
    columnMesh!.setMatrixAt(index, columnMatrix);
  };

  if (layout.mode === "surface") {
    columnGeometry = new THREE.BoxGeometry(1, 1, 1).translate(0, 0.5, 0);
    const normals = columnGeometry.getAttribute("normal");
    const faceColours = new Float32Array(normals.count * 3);
    for (let index = 0; index < normals.count; index++) {
      const light = columnFaceLight(normals.getY(index));
      faceColours[index * 3] = light; faceColours[index * 3 + 1] = light; faceColours[index * 3 + 2] = light;
    }
    columnGeometry.setAttribute("color", new THREE.BufferAttribute(faceColours, 3));
    const material = new THREE.MeshBasicMaterial({ vertexColors: true });
    columnMesh = new THREE.InstancedMesh(columnGeometry, material, layout.marks.length);
    layout.marks.forEach((mark, index) => {
      placeColumn(index);
      columnMesh!.setColorAt(index, storedColour(options.palette, mark.level));
    });
    columnMesh.instanceMatrix.setUsage(THREE.DynamicDrawUsage);
    columnMesh.instanceMatrix.needsUpdate = true;
    if (columnMesh.instanceColor) columnMesh.instanceColor.needsUpdate = true;
    columnMesh.frustumCulled = false;
    columnMesh.userData.rows = markRows;
    materials.push(material);
    scene.add(columnMesh);
  } else {
    const positions = new Float32Array(layout.marks.length * 3);
    pointColours = new Float32Array(layout.marks.length * 4);
    layout.marks.forEach((mark, index) => {
      positionOf.get(mark.row)!.toArray(positions, index * 3);
      storedColour(options.palette, mark.level).toArray(pointColours!, index * 4);
      pointColours![index * 4 + 3] = 1;
    });
    pointGeometry = new THREE.BufferGeometry();
    pointGeometry.setAttribute("position", new THREE.BufferAttribute(positions, 3));
    pointColourAttribute = new THREE.BufferAttribute(pointColours, 4);
    pointColourAttribute.setUsage(THREE.DynamicDrawUsage);
    pointGeometry.setAttribute("companionColor", pointColourAttribute);
    const material = new THREE.PointsNodeMaterial({
      size: 11, sizeAttenuation: false, transparent: true, depthWrite: false, alphaTest: 0.02,
    });
    const colourNode = attribute("companionColor", "vec4");
    // Point textures are a WebGL feature. WebGPU point primitives are one device pixel,
    // while its maintained WebGL 2 fallback uses the same round disc as the prior sprite path.
    if (backend === "WebGPU") material.colorNode = colourNode;
    else {
      const discSample = textureNode(disc, pointUV), ringSample = textureNode(ring, pointUV);
      material.colorNode = vec4(mix(colourNode.rgb, uniform(pointInk), ringSample.a), colourNode.a.mul(discSample.a));
    }
    pointObject = new THREE.Points(pointGeometry, material);
    pointObject.frustumCulled = false;
    pointObject.renderOrder = 2;
    pointObject.userData.rows = markRows;
    materials.push(material);
    scene.add(pointObject);
  }

  // The floor grid uses the same retained-value tick positions as the HTML labels.
  const gridPositions: number[] = [];
  for (const tick of layout.ticks.x) gridPositions.push(tick.at, layout.floor, -0.5, tick.at, layout.floor, 0.5);
  for (const tick of layout.ticks.y) gridPositions.push(-0.5, layout.floor, tick.at, 0.5, layout.floor, tick.at);
  const gridGeometry = new THREE.BufferGeometry();
  gridGeometry.setAttribute("position", new THREE.Float32BufferAttribute(gridPositions, 3));
  const gridMaterial = new THREE.LineBasicMaterial({ transparent: true, opacity: 0.24 });
  const grid = new THREE.LineSegments(gridGeometry, gridMaterial);
  grid.renderOrder = -2;
  scene.add(grid);

  const boxHeight = Math.max(0.01, Math.max(...layout.marks.map((mark) => mark.position[1])) - layout.floor);
  const box = new THREE.BoxGeometry(1, boxHeight, 1);
  const frameGeometry = new THREE.EdgesGeometry(box);
  box.dispose();
  const frameMaterial = new THREE.LineBasicMaterial({ transparent: true, opacity: 0.34 });
  const frame = new THREE.LineSegments(frameGeometry, frameMaterial);
  frame.position.y = layout.floor + boxHeight / 2;
  frame.renderOrder = -1;
  scene.add(frame);

  const makeDynamicMarker = (size: number) => {
    const values = new Float32Array(points.length * 3);
    const attribute = new THREE.InstancedBufferAttribute(values, 3);
    attribute.setUsage(THREE.DynamicDrawUsage);
    const material = new THREE.PointsNodeMaterial({
      color: options.theme().ink, map: ring, size, sizeAttenuation: false,
      transparent: true, depthWrite: false, alphaTest: 0.02,
    });
    material.positionNode = instancedDynamicBufferAttribute(attribute);
    const sprite = new THREE.Sprite(material);
    sprite.count = 0;
    sprite.frustumCulled = false;
    sprite.renderOrder = 4;
    scene.add(sprite);
    materials.push(material);
    return { values, attribute, material, sprite };
  };
  const selectedMarker = makeDynamicMarker(14);
  const activeMarker = makeDynamicMarker(18);

  const ticks = tickLayer(host, layout);
  let selectedRows = new Set<number>();
  let activeRow: number | null = null;
  const markerPosition = (row: number) => {
    const position = positionOf.get(row)?.clone();
    if (position && layout.mode === "surface") position.y += 0.005;
    return position;
  };
  const placeSelected = () => {
    let count = 0;
    for (const row of selectedRows) {
      if (!visibleGroups.has(groupOf.get(row) ?? "")) continue;
      const position = markerPosition(row);
      if (!position) continue;
      position.toArray(selectedMarker.values, count * 3);
      count++;
    }
    selectedMarker.sprite.count = count;
    selectedMarker.attribute.needsUpdate = true;
  };
  const placeActive = () => {
    const position = activeRow === null || !visibleGroups.has(groupOf.get(activeRow) ?? "")
      ? null : markerPosition(activeRow);
    activeMarker.sprite.count = position ? 1 : 0;
    if (position) position.toArray(activeMarker.values, 0);
    activeMarker.attribute.needsUpdate = true;
  };

  let frameRequest = 0;
  const updateTickPositions = () => {
    const { width, height } = host.getBoundingClientRect();
    const nearX = camera.position.x >= controls.target.x ? 1 : -1;
    const nearZ = camera.position.z >= controls.target.z ? 1 : -1;
    const toPixels = (position: THREE.Vector3) => {
      const projected = position.clone().project(camera);
      return { x: ((projected.x + 1) / 2) * width, y: ((1 - projected.y) / 2) * height, z: projected.z };
    };
    for (const label of ticks.labels) {
      if (label.axis === "x") {
        label.position.set(label.at, layout.floor, nearZ * 0.52);
        label.outside.set(label.at, layout.floor, nearZ * 0.66);
      } else if (label.axis === "y") {
        label.position.set(nearX * 0.52, layout.floor, label.at);
        label.outside.set(nearX * 0.66, layout.floor, label.at);
      } else {
        // The height ticks use a camera-relative back edge and move diagonally away from the box.
        label.position.set(nearX * 0.52, label.at, nearZ * -0.52);
        label.outside.set(nearX * 0.66, label.at, nearZ * -0.66);
      }
      const anchor = toPixels(label.position), outside = toPixels(label.outside);
      const shown = anchor.z >= -1 && anchor.z <= 1;
      label.element.hidden = !shown;
      if (!shown) continue;
      const length = Math.hypot(outside.x - anchor.x, outside.y - anchor.y) || 1;
      const nx = (outside.x - anchor.x) / length, ny = (outside.y - anchor.y) / length;
      const offset = 5 + (Math.abs(nx) * label.element.offsetWidth + Math.abs(ny) * label.element.offsetHeight) / 2;
      label.element.style.left = `${anchor.x + nx * offset}px`;
      label.element.style.top = `${anchor.y + ny * offset}px`;
    }
    const occupied: DOMRect[] = [];
    for (const label of ticks.labels) {
      if (label.element.hidden) continue;
      const box = label.element.getBoundingClientRect();
      const overlaps = occupied.some((used) => box.left < used.right + 3 && box.right + 3 > used.left
        && box.top < used.bottom + 2 && box.bottom + 2 > used.top);
      label.element.hidden = overlaps;
      if (!overlaps) occupied.push(box);
    }
  };
  const draw = () => {
    frameRequest = 0;
    if (disposed || canvas.width < 1 || canvas.height < 1 || !host.isConnected) return;
    renderer.render(scene, camera);
    updateTickPositions();
  };
  // Pointer, resize, theme and selection bursts collapse to one display frame. There is no render loop.
  const requestDraw = () => { if (!frameRequest && !disposed) frameRequest = requestAnimationFrame(draw); };
  controls.addEventListener("change", requestDraw);
  let atHome = true;
  const leaveHome = () => { atHome = false; };
  controls.addEventListener("start", leaveHome);

  const applyTheme = () => {
    const theme = options.theme();
    gridMaterial.color.setStyle(theme.grid);
    frameMaterial.color.setStyle(theme.ink);
    pointInk.setStyle(theme.ink);
    selectedMarker.material.color.setStyle(theme.ink);
    activeMarker.material.color.setStyle(theme.ink);
    requestDraw();
  };
  const themeWatcher = new MutationObserver(applyTheme);
  themeWatcher.observe(document.documentElement, { attributes: true, attributeFilter: ["class", "data-theme", "style"] });
  applyTheme();

  let projectionScale: [number, number] = [1, 1];
  const updateProjection = () => {
    camera.updateProjectionMatrix();
    camera.projectionMatrix.elements[0] *= projectionScale[0];
    camera.projectionMatrix.elements[5] *= projectionScale[1];
    camera.projectionMatrixInverse.copy(camera.projectionMatrix).invert();
  };
  const placeHome = () => {
    const fit = fitCompanionCamera(layout.bounds, camera.aspect, camera.fov);
    projectionScale = fit.projectionScale;
    updateProjection();
    camera.position.set(...fit.position);
    controls.target.set(...fit.target);
    controls.minDistance = Math.max(0.25, fit.distance * 0.3);
    controls.maxDistance = fit.distance * 5;
    controls.update();
  };
  placeHome();

  const resize = () => {
    const { width, height } = host.getBoundingClientRect();
    const ratio = renderer.getPixelRatio();
    // WebGPU rejects a zero-sized texture. A hidden or mid-layout host waits for a real size.
    if (Math.floor(width * ratio) < 1 || Math.floor(height * ratio) < 1) return;
    renderer.setSize(width, height, false);
    canvas.style.width = "100%";
    canvas.style.height = "100%";
    camera.aspect = width / height;
    if (atHome) placeHome();
    else {
      projectionScale = fitCompanionCamera(layout.bounds, camera.aspect, camera.fov).projectionScale;
      updateProjection();
      requestDraw();
    }
  };
  const sizeWatcher = new ResizeObserver(resize);
  sizeWatcher.observe(host);
  resize();

  const raycaster = new THREE.Raycaster();
  const pointer = new THREE.Vector2();
  const pointHitRadius = 9;
  const rowAt = (event: PointerEvent): number | null => {
    const box = canvas.getBoundingClientRect();
    if (box.width < 1 || box.height < 1) return null;
    const x = event.clientX - box.left, y = event.clientY - box.top;
    pointer.set((x / box.width) * 2 - 1, 1 - (y / box.height) * 2);
    camera.updateMatrixWorld();
    raycaster.setFromCamera(pointer, camera);
    if (columnMesh) {
      columnMesh.updateMatrixWorld();
      return nearestVisibleInstanceRow(raycaster.intersectObject(columnMesh, false), markRows, markGroups, visibleGroups);
    }
    if (!pointObject) return null;
    pointObject.updateMatrixWorld();
    const radius = Math.hypot(layout.bounds.width, layout.bounds.height, layout.bounds.depth) / 2;
    const farthestDepth = camera.position.distanceTo(controls.target) + radius;
    const projectedX = Math.abs(camera.projectionMatrix.elements[0]) || 1;
    const projectedY = Math.abs(camera.projectionMatrix.elements[5]) || 1;
    const worldPerPixel = Math.max(2 * farthestDepth / (box.width * projectedX), 2 * farthestDepth / (box.height * projectedY));
    raycaster.params.Points.threshold = pointHitRadius * worldPerPixel;
    const intersections = raycaster.intersectObject(pointObject, false);
    return nearestVisiblePointRow(intersections, markRows, markGroups, visibleGroups, (index) => {
      const projected = positionOf.get(markRows[index])!.clone().project(camera);
      const px = ((projected.x + 1) / 2) * box.width, py = ((1 - projected.y) / 2) * box.height;
      return projected.z >= -1 && projected.z <= 1 && Math.hypot(px - x, py - y) <= pointHitRadius;
    });
  };
  let down: { x: number; y: number } | null = null;
  const onMove = (event: PointerEvent) => { if (!event.buttons) options.onHover(rowAt(event)); };
  const onLeave = () => options.onHover(null);
  const onDown = (event: PointerEvent) => { down = { x: event.clientX, y: event.clientY }; };
  const onUp = (event: PointerEvent) => {
    if (!down || Math.hypot(event.clientX - down.x, event.clientY - down.y) > 4 || event.button !== 0) {
      down = null; return;
    }
    down = null;
    const row = rowAt(event);
    if (row !== null) options.onPick(row);
  };
  canvas.addEventListener("pointermove", onMove);
  canvas.addEventListener("pointerleave", onLeave);
  canvas.addEventListener("pointerdown", onDown);
  canvas.addEventListener("pointerup", onUp);

  const turn = (left: number, up: number) => {
    atHome = false;
    const offset = camera.position.clone().sub(controls.target);
    const spherical = new THREE.Spherical().setFromVector3(offset);
    spherical.theta += left;
    spherical.phi = Math.min(Math.PI - 0.05, Math.max(0.05, spherical.phi + up));
    camera.position.copy(controls.target).add(new THREE.Vector3().setFromSpherical(spherical));
    controls.update();
  };
  const zoom = (factor: number) => {
    atHome = false;
    const offset = camera.position.clone().sub(controls.target).multiplyScalar(factor);
    if (offset.length() < controls.minDistance || offset.length() > controls.maxDistance) return;
    camera.position.copy(controls.target).add(offset);
    controls.update();
  };
  const reset = () => { atHome = true; placeHome(); };
  const step = Math.PI / 36;

  return {
    backend,
    mode: layout.mode,
    fallback: layout.fallback,
    key(event) {
      switch (event.key) {
        case "ArrowLeft": turn(step, 0); return true;
        case "ArrowRight": turn(-step, 0); return true;
        case "ArrowUp": turn(0, -step); return true;
        case "ArrowDown": turn(0, step); return true;
        case "+": case "=": zoom(0.9); return true;
        case "-": case "_": zoom(1 / 0.9); return true;
        case "0": reset(); return true;
        default: return false;
      }
    },
    zoom(direction) { zoom(direction === "in" ? 0.9 : 1 / 0.9); },
    reset,
    setVisible(groups) {
      visibleGroups = new Set(groups);
      if (columnMesh) {
        for (let index = 0; index < layout.marks.length; index++) placeColumn(index);
        columnMesh.instanceMatrix.needsUpdate = true;
      }
      if (pointColours && pointColourAttribute) {
        for (let index = 0; index < layout.marks.length; index++)
          pointColours[index * 4 + 3] = visibleGroups.has(markGroups[index]) ? 1 : 0;
        pointColourAttribute.needsUpdate = true;
      }
      placeSelected();
      placeActive();
      requestDraw();
    },
    setActive(row) {
      activeRow = row;
      placeActive();
      requestDraw();
    },
    setSelected(rows) {
      selectedRows = new Set(rows);
      placeSelected();
      requestDraw();
    },
    setBrushMode(enabled) { controls.enabled = !enabled; },
    brush(rect) {
      const { width, height } = host.getBoundingClientRect();
      if (width < 1 || height < 1) return [];
      camera.updateMatrixWorld();
      const rows: number[] = [];
      const projected = new THREE.Vector3();
      for (const mark of layout.marks) {
        if (!visibleGroups.has(mark.group)) continue;
        projected.set(...mark.position).project(camera);
        if (projected.z < -1 || projected.z > 1) continue;
        const x = ((projected.x + 1) / 2) * width, y = ((1 - projected.y) / 2) * height;
        if (x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom) rows.push(mark.row);
      }
      return rows;
    },
    dispose() {
      if (disposed) return;
      disposed = true;
      if (frameRequest) cancelAnimationFrame(frameRequest);
      sizeWatcher.disconnect();
      themeWatcher.disconnect();
      canvas.removeEventListener("pointermove", onMove);
      canvas.removeEventListener("pointerleave", onLeave);
      canvas.removeEventListener("pointerdown", onDown);
      canvas.removeEventListener("pointerup", onUp);
      controls.removeEventListener("change", requestDraw);
      controls.removeEventListener("start", leaveHome);
      controls.dispose();
      for (const material of materials) material.dispose();
      columnGeometry?.dispose();
      pointGeometry?.dispose();
      gridGeometry.dispose(); gridMaterial.dispose();
      frameGeometry.dispose(); frameMaterial.dispose();
      disc.dispose(); ring.dispose();
      renderer.dispose();
      ticks.layer.remove();
      canvas.remove();
      live--;
    },
  };
}
