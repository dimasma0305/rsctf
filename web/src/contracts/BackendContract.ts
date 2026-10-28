/* eslint-disable */
/* tslint:disable */
// @ts-nocheck
/*
 * ---------------------------------------------------------------
 * ## THIS FILE WAS GENERATED VIA SWAGGER-TYPESCRIPT-API        ##
 * ##                                                           ##
 * ## AUTHOR: acacode                                           ##
 * ## SOURCE: https://github.com/acacode/swagger-typescript-api ##
 * ---------------------------------------------------------------
 */

export interface ControlJobModel {
  cancellationRequested: boolean;
  /** @format int32 */
  challengeId?: number | null;
  /**
   * Unix milliseconds.
   * @format int64
   */
  createdAtUtc: number;
  error?: string | null;
  fingerprint: string;
  /**
   * Unix milliseconds, or null before completion.
   * @format int64
   */
  finishedAtUtc?: number | null;
  /** @format int32 */
  gameId: number;
  /** @format uuid */
  id: string;
  kind: string;
  /** @format uuid */
  operationId: string;
  /** @format int32 */
  progressCurrent: number;
  /** @format int32 */
  progressTotal: number;
  /** @format int32 */
  requestedGeneration: number;
  result?: any;
  scopeKey: string;
  status: ControlJobStatus;
  /**
   * Unix milliseconds.
   * @format int64
   */
  updatedAtUtc: number;
}

export type ControlJobStatus =
  | "Queued"
  | "Running"
  | "Succeeded"
  | "Failed"
  | "Cancelled";

export interface ErrorBody {
  /**
   * @format int32
   * @min 0
   */
  status: number;
  title: string;
}
