// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

// Types of apps/web/wasm-translate/lakko_translate.mjs, copied there by
// tools/translate-wasm/build.sh. Written by hand against the embind bindings
// of mozilla/translations inference/wasm/bindings/*.cpp at the commit
// build.sh pins, and tools/translate-wasm/zstd_bindings.cpp + lakko-post.js.
// Only the surface the Lakko worker uses is declared.
//
// Ownership: every object created from JS (new X, a vector's get(), a
// function returning a class) holds C++ memory until its delete() is called.

/** An embind object: C++ memory freed by delete(). */
export interface EmbindObject {
  delete(): void;
}

/** marian::bergamot::AlignedMemory (AlignedVector<char>). */
export interface AlignedMemory extends EmbindObject {
  size(): number;
  /** A view into the module's memory; invalid after the memory grows. */
  getByteArrayView(): Uint8Array;
}

export interface AlignedMemoryConstructor {
  new (bytes: number, alignment: number): AlignedMemory;
}

/** std::vector<AlignedMemory*> (register_vector "AlignedMemoryList"). */
export interface AlignedMemoryList extends EmbindObject {
  push_back(memory: AlignedMemory): void;
  size(): number;
  get(index: number): AlignedMemory | undefined;
}

export interface AlignedMemoryListConstructor {
  new (): AlignedMemoryList;
}

/** std::vector<std::string> (register_vector "VectorString"). */
export interface VectorString extends EmbindObject {
  push_back(text: string): void;
  size(): number;
  get(index: number): string | undefined;
}

export interface VectorStringConstructor {
  new (): VectorString;
}

/** marian::bergamot::ResponseOptions (value_object "ResponseOptions"). */
export interface ResponseOptions {
  qualityScores: boolean;
  alignment: boolean;
  /** Bound to ResponseOptions::HTML. Lakko always passes false (ADR-0100). */
  html: boolean;
}

/** std::vector<ResponseOptions> (register_vector "VectorResponseOptions"). */
export interface VectorResponseOptions extends EmbindObject {
  push_back(options: ResponseOptions): void;
  size(): number;
}

export interface VectorResponseOptionsConstructor {
  new (): VectorResponseOptions;
}

/** marian::bergamot::ByteRange (value_object "ByteRange"). */
export interface ByteRange {
  begin: number;
  end: number;
}

/** marian::bergamot::Response (class "Response"). */
export interface Response extends EmbindObject {
  /** Number of sentences. */
  size(): number;
  getOriginalText(): string;
  getTranslatedText(): string;
  getSourceSentence(sentenceIndex: number): ByteRange;
  getTranslatedSentence(sentenceIndex: number): ByteRange;
}

/** std::vector<Response> (register_vector "VectorResponse"). */
export interface VectorResponse extends EmbindObject {
  size(): number;
  /** A copy of the element (delete() it), or undefined past the end. */
  get(index: number): Response | undefined;
}

/** std::shared_ptr<TranslationModel> (class "TranslationModel"). */
export interface TranslationModel extends EmbindObject {}

export interface TranslationModelConstructor {
  /**
   * Moves the contents of `model`, `lex` and every entry of `vocabs` (one
   * shared vocabulary or source + target) into the model; their handles stay
   * valid but empty and still need delete(). `lex` must not be null: the
   * factory dereferences it.
   */
  new (
    sourceLanguage: string,
    targetLanguage: string,
    configYaml: string,
    model: AlignedMemory,
    lex: AlignedMemory,
    vocabs: AlignedMemoryList,
    qualityModel: AlignedMemory | null,
  ): TranslationModel;
}

/** marian::bergamot::BlockingService::Config (value_object). */
export interface BlockingServiceConfig {
  cacheSize: number;
}

/** std::shared_ptr<BlockingService> (class "BlockingService"). */
export interface BlockingService extends EmbindObject {
  /** BlockingService::translateMultiple. */
  translate(model: TranslationModel, messages: VectorString, options: VectorResponseOptions): VectorResponse;
  /** BlockingService::pivotMultiple: source -> English -> target. */
  translateViaPivoting(
    first: TranslationModel,
    second: TranslationModel,
    messages: VectorString,
    options: VectorResponseOptions,
  ): VectorResponse;
}

export interface BlockingServiceConstructor {
  new (config: BlockingServiceConfig): BlockingService;
}

export interface LakkoTranslateModule {
  AlignedMemory: AlignedMemoryConstructor;
  AlignedMemoryList: AlignedMemoryListConstructor;
  TranslationModel: TranslationModelConstructor;
  BlockingService: BlockingServiceConstructor;
  VectorString: VectorStringConstructor;
  VectorResponseOptions: VectorResponseOptionsConstructor;

  /**
   * Decompresses one zstd frame into a new AlignedMemory of exactly the
   * decompressed size. `expectedSize` is the decompressed size from the model
   * manifest: Mozilla's model files do not state their size in the frame, so
   * for them it is required; when the frame does state one, it must equal
   * `expectedSize`. Throws when neither gives a size, on an empty frame, a
   * size over 512 MiB, data after the frame, output longer or shorter than
   * the size, an invalid alignment, or any decode error; never returns
   * partial output.
   */
  zstdDecompressToAligned(src: Uint8Array, alignment: number, expectedSize?: number): AlignedMemory;

  /**
   * The content size a zstd frame header states, or -1 when the header is
   * invalid, states no size, or states one above 2^53 - 1.
   */
  zstdFrameContentSize(src: Uint8Array): number;

  /** The module's memory; replaced when the memory grows, so read it each time. */
  HEAPU8: Uint8Array;
}

export interface LakkoTranslateModuleArg {
  wasmBinary?: ArrayBuffer | Uint8Array;
  /** Bytes; defaults to 40 MiB. The memory grows from there. */
  INITIAL_MEMORY?: number;
  print?: (text: string) => void;
  printErr?: (text: string) => void;
  onAbort?: (what: unknown) => void;
}

/** Emscripten MODULARIZE factory: resolves once the runtime is ready. */
export default function loadLakkoTranslate(moduleArg?: LakkoTranslateModuleArg): Promise<LakkoTranslateModule>;
