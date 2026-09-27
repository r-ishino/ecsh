//! タスクの大きさ（CPU / メモリ）。人の単位の読み書き、Fargate で指定できる組み合わせ、組み込みの一覧

use std::fmt;
use std::str::FromStr;

use anyhow::{Context, Result, bail};

const UNITS_PER_VCPU: u32 = 1024;
const MIB_PER_GB: u32 = 1024;

/// タスクの CPU。ECS の CPU ユニット（1 vCPU = 1024）で持つ
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cpu(u32);

impl Cpu {
    pub const fn from_units(units: u32) -> Self {
        Self(units)
    }

    pub fn units(self) -> u32 {
        self.0
    }

    /// ECS の API が返す `"1024"` の形
    pub fn from_api(units: &str) -> Result<Self> {
        units
            .parse()
            .map(Self)
            .with_context(|| format!("CPU `{units}` を CPU ユニットとして読めません"))
    }
}

/// `2`、`0.5`、`2vCPU`。単位を省いたら vCPU
impl FromStr for Cpu {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let number = strip_suffix_ignoring_case(text.trim(), &["vcpu"]).unwrap_or(text.trim());
        scaled(number, UNITS_PER_VCPU)
            .map(Self)
            .with_context(|| format!("CPU `{text}` を読めません。{CPU_CHOICES}"))
    }
}

impl fmt::Display for Cpu {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} vCPU", decimal(self.0, UNITS_PER_VCPU))
    }
}

/// タスクのメモリ。MiB で持つ
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Memory(u32);

impl Memory {
    pub const fn from_mib(mib: u32) -> Self {
        Self(mib)
    }

    pub fn mib(self) -> u32 {
        self.0
    }

    /// ECS の API が返す `"2048"`（MiB）の形
    pub fn from_api(mib: &str) -> Result<Self> {
        mib.parse()
            .map(Self)
            .with_context(|| format!("メモリ `{mib}` を MiB として読めません"))
    }
}

/// `8GB`、`0.5GB`、`512MB`。ECS と同じく GB は 1024 MiB。単位は省けない
impl FromStr for Memory {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let trimmed = text.trim();
        let (number, per_unit) = if let Some(number) =
            strip_suffix_ignoring_case(trimmed, &["gib", "gb", "g"])
        {
            (number, MIB_PER_GB)
        } else if let Some(number) = strip_suffix_ignoring_case(trimmed, &["mib", "mb", "m"]) {
            (number, 1)
        } else {
            bail!(
                "メモリ `{text}` に単位がありません。`8GB` や `512MB` のように単位を付けてください"
            );
        };
        scaled(number, per_unit).map(Self).with_context(|| {
            format!("メモリ `{text}` を読めません。`8GB` や `512MB` の形で指定してください")
        })
    }
}

impl fmt::Display for Memory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} GB", decimal(self.0, MIB_PER_GB))
    }
}

fn strip_suffix_ignoring_case<'a>(text: &'a str, suffixes: &[&str]) -> Option<&'a str> {
    suffixes.iter().find_map(|suffix| {
        let split = text.len().checked_sub(suffix.len())?;
        let (number, unit) = (text.get(..split)?, text.get(split..)?);
        unit.eq_ignore_ascii_case(suffix).then(|| number.trim_end())
    })
}

/// `number` × `per_unit` が正の整数になるときだけ返す
fn scaled(number: &str, per_unit: u32) -> Result<u32> {
    let value: f64 = number.parse()?;
    let scaled = value * f64::from(per_unit);
    if !(scaled >= 1.0 && scaled <= f64::from(u32::MAX) && scaled.fract() == 0.0) {
        bail!("{number} は使えない値です");
    }
    Ok(scaled as u32)
}

/// `1536` / 1024 → `1.5`。割り切れない端数は小数 2 桁まで
fn decimal(value: u32, per_unit: u32) -> String {
    if value.is_multiple_of(per_unit) {
        return (value / per_unit).to_string();
    }
    let text = format!("{:.2}", f64::from(value) / f64::from(per_unit));
    text.trim_end_matches('0').to_owned()
}

/// RunTask で上書きするタスクの大きさ
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskSize {
    pub cpu: Cpu,
    pub memory: Memory,
}

impl TaskSize {
    const fn new(cpu_units: u32, memory_mib: u32) -> Self {
        Self {
            cpu: Cpu::from_units(cpu_units),
            memory: Memory::from_mib(memory_mib),
        }
    }
}

impl fmt::Display for TaskSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} / {}", self.cpu, self.memory)
    }
}

/// `--size` の一覧。各 CPU でメモリが 2 倍と 4 倍
pub const PRESETS: [TaskSize; 7] = [
    TaskSize::new(512, 1024),
    TaskSize::new(1024, 2048),
    TaskSize::new(1024, 4096),
    TaskSize::new(2048, 4096),
    TaskSize::new(2048, 8192),
    TaskSize::new(4096, 8192),
    TaskSize::new(4096, 16384),
];

/// ある CPU で Fargate が受け付けるメモリ（MiB）
enum AllowedMemory {
    OneOf(&'static [u32]),
    Range { min: u32, max: u32, step: u32 },
}

impl AllowedMemory {
    fn allows(&self, memory: Memory) -> bool {
        let mib = memory.mib();
        match *self {
            Self::OneOf(choices) => choices.contains(&mib),
            Self::Range { min, max, step } => {
                (min..=max).contains(&mib) && (mib - min).is_multiple_of(step)
            }
        }
    }

    fn describe(&self) -> String {
        let gb = |mib| Memory::from_mib(mib).to_string();
        match *self {
            Self::OneOf(choices) => {
                let choices: Vec<_> = choices.iter().copied().map(gb).collect();
                format!("{} のいずれか", choices.join("・"))
            }
            Self::Range { min, max, step } => format!(
                "{}〜{}（{} 刻み）",
                decimal(min, MIB_PER_GB),
                gb(max),
                gb(step)
            ),
        }
    }
}

const CPU_CHOICES: &str =
    "Fargate で指定できる CPU は 0.25・0.5・1・2・4・8・16 vCPU のいずれかです";

/// Fargate（Linux）のタスクの CPU とメモリの組み合わせ表
fn allowed_memory(cpu: Cpu) -> Option<AllowedMemory> {
    let range = |min_gb, max_gb, step_gb| AllowedMemory::Range {
        min: min_gb * MIB_PER_GB,
        max: max_gb * MIB_PER_GB,
        step: step_gb * MIB_PER_GB,
    };
    Some(match cpu.units() {
        256 => AllowedMemory::OneOf(&[512, 1024, 2048]),
        512 => range(1, 4, 1),
        1024 => range(2, 8, 1),
        2048 => range(4, 16, 1),
        4096 => range(8, 30, 1),
        8192 => range(16, 60, 4),
        16384 => range(32, 120, 8),
        _ => return None,
    })
}

/// Fargate の組み合わせ表に無い大きさを、RunTask の前に弾く
pub fn validate(size: TaskSize) -> Result<()> {
    let Some(allowed) = allowed_memory(size.cpu) else {
        bail!("{} は指定できません。{CPU_CHOICES}", size.cpu);
    };
    if !allowed.allows(size.memory) {
        bail!(
            "{} で指定できるメモリは {}です（{} は指定できません）",
            size.cpu,
            allowed.describe(),
            size.memory
        );
    }
    Ok(())
}

/// タスク定義に書かれた大きさ。EC2 向けのタスク定義では無いこともある
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DefinedSize {
    pub cpu: Option<Cpu>,
    pub memory: Option<Memory>,
}

impl DefinedSize {
    fn as_task_size(self) -> Option<TaskSize> {
        Some(TaskSize {
            cpu: self.cpu?,
            memory: self.memory?,
        })
    }
}

impl fmt::Display for DefinedSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.cpu, self.memory) {
            (Some(cpu), Some(memory)) => write!(f, "{cpu} / {memory}"),
            (Some(cpu), None) => write!(f, "{cpu} / メモリの指定なし"),
            (None, Some(memory)) => write!(f, "CPU の指定なし / {memory}"),
            (None, None) => f.write_str("CPU / メモリの指定なし"),
        }
    }
}

/// 利用者が起動時に求めた大きさ
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeRequest {
    /// 何も指定していない。タスク定義のまま
    Keep,
    /// `--size`。一覧から選ばせる
    Choose,
    /// `--cpu` / `--memory`。片方だけならもう片方はタスク定義の値
    Custom {
        cpu: Option<Cpu>,
        memory: Option<Memory>,
    },
}

/// `--cpu` / `--memory` の足りない方をタスク定義で埋め、組み合わせ表で確かめる
pub fn resolve_custom(
    cpu: Option<Cpu>,
    memory: Option<Memory>,
    defined: DefinedSize,
) -> Result<TaskSize> {
    let size = TaskSize {
        cpu: cpu.or(defined.cpu).context(
            "タスク定義に CPU の指定が無いので、--memory だけでは大きさを決められません。--cpu も指定してください",
        )?,
        memory: memory.or(defined.memory).context(
            "タスク定義にメモリの指定が無いので、--cpu だけでは大きさを決められません。--memory も指定してください",
        )?,
    };
    validate(size)?;
    Ok(size)
}

/// 選んだ大きさがタスク定義と同じなら、上書きしない（None）
pub fn differs_from(size: TaskSize, defined: DefinedSize) -> Option<TaskSize> {
    (defined.as_task_size() != Some(size)).then_some(size)
}

/// タスク定義のコンテナ 1 つに書かれた CPU / メモリ。None は指定なし
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerLimits {
    pub name: String,
    pub cpu: Option<Cpu>,
    pub memory: Option<Memory>,
    pub memory_reservation: Option<Memory>,
}

impl ContainerLimits {
    /// ECS がタスクのメモリから差し引く量。上限が無ければ予約
    fn memory_claim(&self) -> u32 {
        self.memory
            .or(self.memory_reservation)
            .map_or(0, Memory::mib)
    }
}

/// タスク定義のうち、大きさの上書きに関わる部分
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    pub size: DefinedSize,
    /// プロファイルの container
    pub container: ContainerLimits,
    /// それ以外のコンテナ（サイドカー）
    pub others: Vec<ContainerLimits>,
}

/// プロファイルの container の containerOverrides に足す CPU / メモリ。None は上書きしない
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ContainerResize {
    pub cpu: Option<Cpu>,
    pub memory: Option<Memory>,
    pub memory_reservation: Option<Memory>,
}

/// RunTask で上書きする大きさ一式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeOverride {
    pub task: TaskSize,
    pub container: ContainerResize,
}

/// タスクの大きさを変えるとき、プロファイルの container の CPU / メモリをどう合わせるか
///
/// タスクだけを上書きすると、コンテナに memory（上限）が書かれていれば上限はそのままで、増やしたメモリを使えずに OOM で落ちる。
/// 減らしたときは、コンテナの値の合計がタスクを超えて RunTask が失敗する。
/// なので、コンテナに書かれている値だけを「タスクからサイドカーの分を引いた残り」に合わせる。サイドカーは変えない
pub fn plan(task: TaskSize, definition: &Definition) -> Result<SizeOverride> {
    let Definition {
        container, others, ..
    } = definition;
    let mut resize = ContainerResize::default();

    if container.memory.is_some() || container.memory_reservation.is_some() {
        let others_memory =
            Memory::from_mib(others.iter().map(ContainerLimits::memory_claim).sum());
        let fitted = remaining(task.memory.mib(), others_memory.mib())
            .map(Memory::from_mib)
            .with_context(|| {
                format!(
                    "コンテナ `{}` 以外のメモリが合計 {others_memory} あり、{} ではコンテナ `{}` に残りません",
                    container.name, task.memory, container.name
                )
            })?;
        resize.memory = container.memory.map(|_| fitted);
        resize.memory_reservation = container
            .memory_reservation
            .filter(|reservation| reservation.mib() > fitted.mib())
            .map(|_| fitted);
    }

    if container.cpu.is_some() {
        let others_cpu = Cpu::from_units(
            others
                .iter()
                .filter_map(|other| other.cpu)
                .map(Cpu::units)
                .sum(),
        );
        let fitted = remaining(task.cpu.units(), others_cpu.units())
            .map(Cpu::from_units)
            .with_context(|| {
                format!(
                    "コンテナ `{}` 以外の CPU が合計 {others_cpu} あり、{} ではコンテナ `{}` に残りません",
                    container.name, task.cpu, container.name
                )
            })?;
        resize.cpu = Some(fitted);
    }

    Ok(SizeOverride {
        task,
        container: resize,
    })
}

fn remaining(total: u32, used: u32) -> Option<u32> {
    total.checked_sub(used).filter(|&rest| rest > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(cpu: &str, memory: &str) -> TaskSize {
        TaskSize {
            cpu: cpu.parse().unwrap(),
            memory: memory.parse().unwrap(),
        }
    }

    #[test]
    fn cpu_is_read_in_vcpu_with_or_without_the_unit() {
        for (text, units) in [
            ("2", 2048),
            ("0.5", 512),
            ("0.25", 256),
            ("16", 16384),
            ("1vCPU", 1024),
            ("1 vcpu", 1024),
        ] {
            assert_eq!(text.parse::<Cpu>().unwrap().units(), units, "{text:?}");
        }
    }

    #[test]
    fn cpu_that_is_not_a_positive_number_of_units_is_rejected() {
        for text in ["", "abc", "0", "-1", "0.0001", "2GB"] {
            assert!(text.parse::<Cpu>().is_err(), "{text:?}");
        }
    }

    #[test]
    fn memory_is_read_in_gb_or_mb_where_gb_is_1024_mib() {
        for (text, mib) in [
            ("8GB", 8192),
            ("8gb", 8192),
            ("8 GB", 8192),
            ("8G", 8192),
            ("8GiB", 8192),
            ("0.5GB", 512),
            ("1.5GB", 1536),
            ("512MB", 512),
            ("512m", 512),
            ("512MiB", 512),
        ] {
            assert_eq!(text.parse::<Memory>().unwrap().mib(), mib, "{text:?}");
        }
    }

    #[test]
    fn memory_without_a_unit_is_rejected_because_mb_and_gb_would_be_ambiguous() {
        let message = "8".parse::<Memory>().unwrap_err().to_string();

        assert!(message.contains("単位"), "{message}");
    }

    #[test]
    fn memory_that_is_not_a_whole_number_of_mib_is_rejected() {
        for text in ["GB", "0GB", "-1GB", "0.5MB", "xGB"] {
            assert!(text.parse::<Memory>().is_err(), "{text:?}");
        }
    }

    #[test]
    fn sizes_are_shown_in_vcpu_and_gb() {
        assert_eq!(size("2", "8GB").to_string(), "2 vCPU / 8 GB");
        assert_eq!(size("0.25", "512MB").to_string(), "0.25 vCPU / 0.5 GB");
        assert_eq!(Memory::from_mib(1536).to_string(), "1.5 GB");
    }

    #[test]
    fn api_values_are_read_as_cpu_units_and_mib() {
        assert_eq!(Cpu::from_api("1024").unwrap(), Cpu::from_units(1024));
        assert_eq!(Memory::from_api("2048").unwrap(), Memory::from_mib(2048));
        assert!(Cpu::from_api("1 vCPU").is_err());
    }

    #[test]
    fn every_preset_is_a_valid_fargate_size_listed_from_small_to_large() {
        for preset in PRESETS {
            validate(preset).unwrap();
        }
        let labels: Vec<_> = PRESETS.iter().map(ToString::to_string).collect();
        assert_eq!(
            labels,
            [
                "0.5 vCPU / 1 GB",
                "1 vCPU / 2 GB",
                "1 vCPU / 4 GB",
                "2 vCPU / 4 GB",
                "2 vCPU / 8 GB",
                "4 vCPU / 8 GB",
                "4 vCPU / 16 GB",
            ]
        );
    }

    #[test]
    fn fargate_accepts_the_edges_of_each_cpu_range_in_its_steps() {
        for (cpu, memory) in [
            ("0.25", "512MB"),
            ("0.25", "2GB"),
            ("0.5", "4GB"),
            ("1", "2GB"),
            ("1", "8GB"),
            ("2", "16GB"),
            ("4", "30GB"),
            ("8", "60GB"),
            ("16", "32GB"),
            ("16", "120GB"),
        ] {
            assert!(validate(size(cpu, memory)).is_ok(), "{cpu} / {memory}");
        }
    }

    #[test]
    fn memory_outside_the_range_for_the_cpu_is_rejected_with_the_allowed_range() {
        let message = validate(size("1", "16GB")).unwrap_err().to_string();

        assert_eq!(
            message,
            "1 vCPU で指定できるメモリは 2〜8 GB（1 GB 刻み）です（16 GB は指定できません）"
        );
    }

    #[test]
    fn memory_between_the_steps_is_rejected() {
        assert!(validate(size("1", "2.5GB")).is_err());
        assert!(validate(size("8", "18GB")).is_err());
        assert!(validate(size("0.25", "1.5GB")).is_err());
    }

    #[test]
    fn quarter_vcpu_lists_its_three_memory_choices() {
        let message = validate(size("0.25", "4GB")).unwrap_err().to_string();

        assert!(
            message.contains("0.5 GB・1 GB・2 GB のいずれか"),
            "{message}"
        );
    }

    #[test]
    fn cpu_not_offered_by_fargate_is_rejected_with_the_choices() {
        let message = validate(size("3", "8GB")).unwrap_err().to_string();

        assert!(message.contains("3 vCPU は指定できません"), "{message}");
        assert!(
            message.contains("0.25・0.5・1・2・4・8・16 vCPU"),
            "{message}"
        );
    }

    const DEFINED: DefinedSize = DefinedSize {
        cpu: Some(Cpu::from_units(1024)),
        memory: Some(Memory::from_mib(2048)),
    };

    #[test]
    fn custom_size_takes_the_missing_half_from_the_task_definition() {
        let cpu_only = resolve_custom(Some("0.5".parse().unwrap()), None, DEFINED).unwrap();
        let memory_only = resolve_custom(None, Some("4GB".parse().unwrap()), DEFINED).unwrap();

        assert_eq!(cpu_only, size("0.5", "2GB"));
        assert_eq!(memory_only, size("1", "4GB"));
    }

    #[test]
    fn custom_size_filled_from_the_task_definition_is_still_checked_against_fargate() {
        let message = resolve_custom(Some("4".parse().unwrap()), None, DEFINED)
            .unwrap_err()
            .to_string();

        assert!(
            message.starts_with("4 vCPU で指定できるメモリは 8〜30 GB"),
            "{message}"
        );
    }

    #[test]
    fn custom_size_needs_both_when_the_task_definition_has_no_size() {
        let message = resolve_custom(Some("1".parse().unwrap()), None, DefinedSize::default())
            .unwrap_err()
            .to_string();

        assert!(message.contains("--memory も指定してください"), "{message}");
    }

    #[test]
    fn size_equal_to_the_task_definition_is_not_an_override() {
        assert_eq!(differs_from(size("1", "2GB"), DEFINED), None);
        assert_eq!(
            differs_from(size("2", "4GB"), DEFINED),
            Some(size("2", "4GB"))
        );
        assert_eq!(
            differs_from(size("1", "2GB"), DefinedSize::default()),
            Some(size("1", "2GB"))
        );
    }

    #[test]
    fn task_definition_size_is_shown_even_when_partly_missing() {
        assert_eq!(DEFINED.to_string(), "1 vCPU / 2 GB");
        assert_eq!(DefinedSize::default().to_string(), "CPU / メモリの指定なし");
    }

    /// cpu の 0 は指定なし
    fn container(
        cpu: u32,
        memory: Option<u32>,
        memory_reservation: Option<u32>,
    ) -> ContainerLimits {
        ContainerLimits {
            name: "app".into(),
            cpu: (cpu > 0).then_some(Cpu::from_units(cpu)),
            memory: memory.map(Memory::from_mib),
            memory_reservation: memory_reservation.map(Memory::from_mib),
        }
    }

    fn sidecar(cpu: u32, memory: Option<u32>, memory_reservation: Option<u32>) -> ContainerLimits {
        ContainerLimits {
            name: "sidecar".into(),
            ..container(cpu, memory, memory_reservation)
        }
    }

    fn definition(container: ContainerLimits, others: Vec<ContainerLimits>) -> Definition {
        Definition {
            size: DEFINED,
            container,
            others,
        }
    }

    #[test]
    fn container_without_its_own_limits_is_left_to_the_task_size() {
        let planned = plan(
            size("2", "8GB"),
            &definition(container(0, None, None), vec![]),
        )
        .unwrap();

        assert_eq!(planned.container, ContainerResize::default());
        assert_eq!(planned.task, size("2", "8GB"));
    }

    #[test]
    fn container_memory_limit_follows_the_task_minus_sidecars_so_a_larger_task_is_usable() {
        let definition = definition(
            container(0, Some(1536), None),
            vec![sidecar(0, Some(256), None), sidecar(0, None, Some(256))],
        );

        let planned = plan(size("2", "8GB"), &definition).unwrap();

        assert_eq!(planned.container.memory, Some(Memory::from_mib(8192 - 512)));
        assert_eq!(planned.container.memory_reservation, None);
    }

    #[test]
    fn container_memory_limit_shrinks_with_a_smaller_task() {
        let planned = plan(
            size("0.5", "1GB"),
            &definition(container(0, Some(2048), None), vec![]),
        )
        .unwrap();

        assert_eq!(planned.container.memory, Some(Memory::from_mib(1024)));
    }

    #[test]
    fn container_memory_reservation_is_lowered_only_when_it_no_longer_fits() {
        let fits = plan(
            size("2", "8GB"),
            &definition(container(0, None, Some(1024)), vec![]),
        )
        .unwrap();
        let too_large = plan(
            size("0.5", "1GB"),
            &definition(container(0, Some(2048), Some(1536)), vec![]),
        )
        .unwrap();

        assert_eq!(fits.container, ContainerResize::default());
        assert_eq!(too_large.container.memory, Some(Memory::from_mib(1024)));
        assert_eq!(
            too_large.container.memory_reservation,
            Some(Memory::from_mib(1024))
        );
    }

    #[test]
    fn container_cpu_follows_the_task_minus_sidecars() {
        let planned = plan(
            size("2", "4GB"),
            &definition(container(1024, None, None), vec![sidecar(256, None, None)]),
        )
        .unwrap();

        assert_eq!(planned.container.cpu, Some(Cpu::from_units(2048 - 256)));
    }

    #[test]
    fn task_too_small_for_the_sidecars_is_rejected_before_run_task() {
        let definition = definition(
            container(0, Some(512), None),
            vec![sidecar(0, Some(1024), None)],
        );

        let message = plan(size("0.5", "1GB"), &definition)
            .unwrap_err()
            .to_string();

        assert!(message.contains("コンテナ `app` に残りません"), "{message}");
    }
}
