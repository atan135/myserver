export interface ActivityControlService {
  list(query: Record<string, unknown>): Promise<unknown>;
  detail(activityId: string): Promise<unknown>;
  createDraft(command: Record<string, unknown>): Promise<unknown>;
  updateDraft(activityId: string, command: Record<string, unknown>): Promise<unknown>;
  preflight(activityId: string, command: Record<string, unknown>): Promise<unknown>;
  publish(activityId: string, command: Record<string, unknown>): Promise<unknown>;
  offline(activityId: string, command: Record<string, unknown>): Promise<unknown>;
  records(activityId: string, query: Record<string, unknown>): Promise<unknown>;
}

export class ActivityControlUnavailableService implements ActivityControlService {
  private unavailable(): never {
    const error: any = new Error("ACTIVITY_CONTROL_UNAVAILABLE");
    error.code = "ACTIVITY_CONTROL_UNAVAILABLE";
    throw error;
  }
  async list(): Promise<unknown> { return this.unavailable(); }
  async detail(): Promise<unknown> { return this.unavailable(); }
  async createDraft(): Promise<unknown> { return this.unavailable(); }
  async updateDraft(): Promise<unknown> { return this.unavailable(); }
  async preflight(): Promise<unknown> { return this.unavailable(); }
  async publish(): Promise<unknown> { return this.unavailable(); }
  async offline(): Promise<unknown> { return this.unavailable(); }
  async records(): Promise<unknown> { return this.unavailable(); }
}
