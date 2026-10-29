import assert from "node:assert/strict";
import test from "node:test";
import * as THREE from "three";
import { nearestVisibleInstanceRow } from "./companion3d_pick.ts";

test("raycasting overlapping columns returns the nearest drawn instance", () => {
  const geometry = new THREE.BoxGeometry(1, 1, 1);
  const material = new THREE.MeshBasicMaterial();
  const columns = new THREE.InstancedMesh(geometry, material, 2);
  columns.setMatrixAt(0, new THREE.Matrix4().makeTranslation(0, 0, -1));
  columns.setMatrixAt(1, new THREE.Matrix4().makeTranslation(0, 0, 1));
  columns.instanceMatrix.needsUpdate = true;
  columns.updateMatrixWorld(true);

  const camera = new THREE.PerspectiveCamera(40, 1, 0.01, 20);
  camera.position.set(0, 0, 5);
  camera.lookAt(0, 0, 0);
  camera.updateProjectionMatrix();
  camera.updateMatrixWorld(true);
  const raycaster = new THREE.Raycaster();
  raycaster.setFromCamera(new THREE.Vector2(0, 0), camera);
  const intersections = raycaster.intersectObject(columns, false);

  assert.deepEqual(intersections.slice(0, 2).map((hit) => hit.instanceId), [1, 1]);
  assert.equal(nearestVisibleInstanceRow(intersections, [41, 82], ["far", "near"], new Set(["far", "near"])), 82);
  assert.equal(nearestVisibleInstanceRow(intersections, [41, 82], ["far", "near"], new Set(["far"])), 41);

  geometry.dispose();
  material.dispose();
});
