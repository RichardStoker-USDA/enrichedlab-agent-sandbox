// How a drawing looks to a reader with a colour vision deficiency (Glass Box, results rule 4): the matrices of Machado,
// Oliveira and Fernandes (2009) at full severity, and luminance alone for no colour vision, applied in linear RGB by an
// SVG filter over R's drawing. It is a simulation for checking the colours; the figure itself does not change.
export interface Vision { id: string; label: string; matrix: number[] | null }

export const VISIONS: Vision[] = [
  { id: "typical", label: "Typical colour vision", matrix: null },
  { id: "deuteranopia", label: "Deuteranopia (red-green)", matrix: [0.367322, 0.860646, -0.227968, 0.280085, 0.672501, 0.047413, -0.01182, 0.04294, 0.968881] },
  { id: "protanopia", label: "Protanopia (red-green)", matrix: [0.152286, 1.052583, -0.204868, 0.114503, 0.786281, 0.099216, -0.003882, -0.048116, 1.051998] },
  { id: "tritanopia", label: "Tritanopia (blue-yellow)", matrix: [1.255528, -0.076749, -0.178779, -0.078411, 0.930809, 0.147602, 0.004733, 0.691367, 0.3039] },
  { id: "achromatopsia", label: "No colour vision", matrix: [0.2126, 0.7152, 0.0722, 0.2126, 0.7152, 0.0722, 0.2126, 0.7152, 0.0722] },
];

/** A 3 by 3 matrix as the 4 by 5 values of an SVG feColorMatrix, with alpha kept as it is. */
export function colorMatrixValues(m: number[]): string {
  const row = (i: number) => [m[i * 3], m[i * 3 + 1], m[i * 3 + 2], 0, 0];
  return [...row(0), ...row(1), ...row(2), 0, 0, 0, 1, 0].join(" ");
}
