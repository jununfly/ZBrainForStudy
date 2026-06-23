// v0.38 T_W: pack-driven expert types parity tests.
//
// Pins the contract that expertTypesFromPack(zbrain-base) returns the
// pre-v0.38 hardcoded DEFAULT_TYPES = ['person', 'company']. User packs
// override by setting expert_routing: true on different types.

import { describe, expect, test } from 'bun:test';
import {
  expertTypesFromPack,
  expertTypesFromPackOrThrow,
  parseSchemaPackManifest,
  loadPackFromFile,
} from '../../src/core/schema-pack/index.ts';
import { join } from 'node:path';

const ZBRAIN_BASE_PATH = join(import.meta.dir, '../src/core/schema-pack/base/zbrain-base.yaml');

describe('expertTypesFromPack (T_W) — zbrain-base parity', () => {
  test('zbrain-base returns [person, company]', () => {
    const pack = loadPackFromFile(ZBRAIN_BASE_PATH);
    const types = expertTypesFromPack(pack);
    expect(types.sort()).toEqual(['company', 'person']);
  });

  test('research-shaped pack returns researcher + principal-investigator', () => {
    const pack = parseSchemaPackManifest({
      api_version: 'zbrain-schema-pack-v1',
      name: 'research-state',
      version: '0.1.0',
      extends: null,
      page_types: [
        { name: 'researcher', primitive: 'entity', path_prefixes: ['researchers/'], aliases: [], extractable: true, expert_routing: true },
        { name: 'principal-investigator', primitive: 'entity', path_prefixes: ['pis/'], aliases: ['researcher'], extractable: true, expert_routing: true },
        { name: 'paper', primitive: 'media', path_prefixes: ['papers/'], aliases: [], extractable: false, expert_routing: false },
        { name: 'method', primitive: 'concept', path_prefixes: ['methods/'], aliases: [], extractable: false, expert_routing: false },
      ],
      link_types: [],
    });
    const types = expertTypesFromPack(pack);
    expect(types).toEqual(['researcher', 'principal-investigator']);
  });

  test('preserves declaration order from manifest', () => {
    const pack = parseSchemaPackManifest({
      api_version: 'zbrain-schema-pack-v1',
      name: 'tests/unit',
      version: '0.1.0',
      extends: null,
      page_types: [
        { name: 'zebra', primitive: 'entity', path_prefixes: [], aliases: [], extractable: false, expert_routing: true },
        { name: 'apple', primitive: 'entity', path_prefixes: [], aliases: [], extractable: false, expert_routing: false },
        { name: 'mango', primitive: 'entity', path_prefixes: [], aliases: [], extractable: false, expert_routing: true },
      ],
      link_types: [],
    });
    // NOT sorted: declaration order is preserved (zebra before mango).
    expect(expertTypesFromPack(pack)).toEqual(['zebra', 'mango']);
  });

  test('pack with no expert_routing types returns empty array', () => {
    const pack = parseSchemaPackManifest({
      api_version: 'zbrain-schema-pack-v1',
      name: 'media-only',
      version: '0.1.0',
      extends: null,
      page_types: [
        { name: 'article', primitive: 'media', path_prefixes: [], aliases: [], extractable: false, expert_routing: false },
        { name: 'book', primitive: 'media', path_prefixes: [], aliases: [], extractable: false, expert_routing: false },
      ],
      link_types: [],
    });
    expect(expertTypesFromPack(pack)).toEqual([]);
  });

  test('expertTypesFromPackOrThrow throws on empty', () => {
    const pack = parseSchemaPackManifest({
      api_version: 'zbrain-schema-pack-v1',
      name: 'media-only',
      version: '0.1.0',
      extends: null,
      page_types: [
        { name: 'article', primitive: 'media', path_prefixes: [], aliases: [], extractable: false, expert_routing: false },
      ],
      link_types: [],
    });
    expect(() => expertTypesFromPackOrThrow(pack)).toThrow(/declares no types with expert_routing/);
  });

  test('expertTypesFromPackOrThrow passes when types exist', () => {
    const pack = loadPackFromFile(ZBRAIN_BASE_PATH);
    expect(() => expertTypesFromPackOrThrow(pack)).not.toThrow();
  });
});
