/** A config. */
export class Config {
  name: string;

  load(path: string): Config {
    return new Config();
  }
}

export interface Shape {
  area(): number;
}

export enum Color {
  Red,
}

export type Id = string | number;

export const MAX = 10;

export function main(): void {}
