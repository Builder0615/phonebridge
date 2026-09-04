/**
 * 会话状态展示映射：状态名 → 中文标签 / 语义等级（纯映射，可测试）。
 */

import type {
  ControlState,
  MirrorState,
  SessionStateName,
} from "./types"

export type StatusTone = "default" | "muted" | "success" | "warning" | "destructive" | "info"

export interface StateLabel {
  label: string
  tone: StatusTone
  /** 简短说明（状态栏 / 无障碍描述） */
  hint: string
}

export const SESSION_LABELS: Record<SessionStateName, StateLabel> = {
  idle: { label: "未连接", tone: "muted", hint: "等待开始镜像会话" },
  checking: { label: "检查中", tone: "info", hint: "正在检查依赖与网络" },
  ready: { label: "已就绪", tone: "info", hint: "依赖检查通过，等待启动接收器" },
  mirroring: { label: "连接中", tone: "info", hint: "正在建立 AirPlay 镜像会话" },
  mirroring_connected: { label: "已连接", tone: "success", hint: "镜像画面已连接（控制未启用）" },
  control_pairing: { label: "等待配对", tone: "info", hint: "BLE 广播中，请在 iPhone 端完成配对" },
  control_ready: { label: "控制已连接", tone: "success", hint: "BLE HID 已配对并可输入" },
  reconnecting: { label: "正在重连", tone: "warning", hint: "网络或接收器短暂中断，按策略重试" },
  stopping: { label: "停止中", tone: "muted", hint: "正在释放输入并清理资源" },
  failed: { label: "失败", tone: "destructive", hint: "会话失败，可查看错误并重试" },
}

export const CONTROL_LABELS: Record<ControlState, StateLabel> = {
  disabled: { label: "控制未启用", tone: "muted", hint: "BLE 广播未开启" },
  broadcasting: { label: "广播中", tone: "info", hint: "正在广播 BLE 输入设备" },
  waiting_pairing: { label: "等待配对", tone: "warning", hint: "请在 iPhone 蓝牙中配对" },
  connected: { label: "输入可用", tone: "success", hint: "BLE HID 已连接" },
  input_paused: { label: "输入已暂停", tone: "warning", hint: "输入转发已暂停（Ctrl+Alt+Pause）" },
}

export const MIRROR_LABELS: Record<MirrorState, StateLabel> = {
  idle: { label: "无画面", tone: "muted", hint: "尚未建立镜像" },
  starting: { label: "启动中", tone: "info", hint: "接收器启动中" },
  connecting: { label: "连接中", tone: "info", hint: "等待 iPhone 连接" },
  streaming: { label: "播放中", tone: "success", hint: "画面正在播放" },
  reconnecting: { label: "重连中", tone: "warning", hint: "网络短暂中断" },
  stopped: { label: "已停止", tone: "muted", hint: "镜像已停止" },
  error: { label: "画面错误", tone: "destructive", hint: "镜像引擎异常" },
}

export function sessionLabel(state: SessionStateName): StateLabel {
  return SESSION_LABELS[state] ?? { label: state, tone: "muted", hint: "" }
}

export function controlLabel(state: ControlState): StateLabel {
  return CONTROL_LABELS[state] ?? { label: state, tone: "muted", hint: "" }
}

export function mirrorLabel(state: MirrorState): StateLabel {
  return MIRROR_LABELS[state] ?? { label: state, tone: "muted", hint: "" }
}