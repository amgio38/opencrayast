/** A config. */
export class Config {
  name: string;

  /** Load it. */
  static load(path: string): Config {
    return new Config();
  }

  validate(): boolean {
    return true;
  }
}

export interface Shape {
  area(): number;
}

export enum Color {
  Red,
  Green,
}

export type Id = string | number;

export const MAX = 10;

export function main(): void {}

namespace Util {
  export function help() {}
}
