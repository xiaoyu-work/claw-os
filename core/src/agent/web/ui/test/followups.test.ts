import { expect,test } from "bun:test";
import {readFollowUps} from "../src/lib/followups";

test("follow-up controls exclude current work and validate the queue owner view",()=>{
  const value={task_id:"current",session_id:"session-1",truncated:false,tasks:[{
    id:"next",prompt:"Then check updates",status:"pending",after_task_id:"current",created_at:"2026-10-12T12:00:00Z",
  }]};
  expect(readFollowUps(value,"current").tasks[0].prompt).toBe("Then check updates");
  expect(()=>readFollowUps(value,"another")).toThrow();
  expect(()=>readFollowUps({...value,tasks:[{...value.tasks[0],id:"current"}]},"current")).toThrow();
});
