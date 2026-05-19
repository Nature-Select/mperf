import { Input, InputNumber, Radio, Select, Typography } from '@arco-design/web-react'

const { Text } = Typography
const RadioGroup = Radio.Group
const Option = Select.Option

/// All controls below are disabled placeholders — the actual floating-
/// window overlay, in-recording thresholds, and upload pipeline aren't
/// wired up yet. The layout mirrors PerfDog so users have a stable
/// mental model of what's coming.
export function SettingsTab() {
  return (
    <div style={{ padding: '4px 16px 16px' }}>
      <div
        style={{
          fontSize: 11,
          color: 'var(--color-text-3)',
          padding: '6px 8px',
          marginBottom: 12,
          border: '1px dashed var(--color-border-2)',
          borderRadius: 3,
          lineHeight: 1.5,
        }}
      >
        以下设置项尚未实现，仅作 UI 预览
      </div>

      <Section title="参数设置">
        <Row label="FPS (>=)">
          <StackedInputs>
            <InputNumber size="mini" disabled defaultValue={18} style={{ width: 76 }} />
            <InputNumber size="mini" disabled defaultValue={25} style={{ width: 76 }} />
          </StackedInputs>
        </Row>
        <Row label="FrameTime (>=)">
          <InlineUnit unit="ms">
            <InputNumber size="mini" disabled defaultValue={100} style={{ width: 76 }} />
          </InlineUnit>
        </Row>
        <Row label="CPU (<=)">
          <StackedInputs>
            <InlineUnit unit="%">
              <InputNumber size="mini" disabled defaultValue={60} style={{ width: 76 }} />
            </InlineUnit>
            <InlineUnit unit="%">
              <InputNumber size="mini" disabled defaultValue={80} style={{ width: 76 }} />
            </InlineUnit>
          </StackedInputs>
        </Row>
      </Section>

      <Section title="上传设置">
        <Row label="上传地址">
          <Input
            size="mini"
            disabled
            defaultValue="https://"
            style={{ width: 130 }}
          />
        </Row>
        <Row label="上传格式">
          <Select size="mini" disabled defaultValue="JSON" style={{ width: 100 }}>
            <Option value="JSON">JSON</Option>
            <Option value="CSV">CSV</Option>
          </Select>
        </Row>
        <Row label="自动上传">
          <RadioGroup type="button" size="mini" disabled defaultValue="off">
            <Radio value="on">开</Radio>
            <Radio value="off">关</Radio>
          </RadioGroup>
        </Row>
      </Section>

      <div style={{ marginTop: 12 }}>
        <Text type="secondary" style={{ fontSize: 11 }}>
          应用版本、日志/数据目录等诊断信息见{' '}
          <Text style={{ fontSize: 11, color: 'var(--color-text-2)' }} bold>
            关于
          </Text>{' '}
          标签。
        </Text>
      </div>
    </div>
  )
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div style={{ marginBottom: 14 }}>
      <div
        style={{
          fontSize: 12,
          fontWeight: 600,
          color: 'var(--color-text-2)',
          paddingBottom: 4,
          marginBottom: 6,
          borderBottom: '1px solid var(--color-border-1)',
        }}
      >
        {title}
      </div>
      <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>{children}</div>
    </div>
  )
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div
      style={{
        display: 'grid',
        gridTemplateColumns: '88px 1fr',
        alignItems: 'center',
        columnGap: 8,
        minHeight: 24,
      }}
    >
      <Text type="secondary" style={{ fontSize: 11 }}>
        {label}
      </Text>
      <div>{children}</div>
    </div>
  )
}

function StackedInputs({ children }: { children: React.ReactNode }) {
  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>{children}</div>
  )
}

function InlineUnit({ unit, children }: { unit: string; children: React.ReactNode }) {
  return (
    <div style={{ display: 'flex', alignItems: 'center', gap: 4 }}>
      {children}
      <span style={{ fontSize: 11, color: 'var(--color-text-3)' }}>{unit}</span>
    </div>
  )
}
