#![no_std]
#![no_main]

extern crate alloc;
#[macro_use]
extern crate libax;

#[cfg(target_arch = "riscv64")]
use dtb_riscv64::MachineMeta;
#[cfg(target_arch = "aarch64")]
use dtb_aarch64::MachineMeta;
#[cfg(target_arch = "aarch64")]
use aarch64_config::GUEST_KERNEL_BASE_VADDR;
#[cfg(target_arch = "aarch64")]
use libax::{
    hv::{
        self, GuestPageTable, GuestPageTableTrait, HyperCraftHalImpl, PerCpu,
        Result, VCpu, VmCpus, VM,
    },
    info,
};
#[cfg(not(target_arch = "aarch64"))]
use libax::{
    hv::{
        self, GuestPageTable, GuestPageTableTrait, HyperCallMsg, HyperCraftHalImpl, PerCpu, Result,
        VCpu, VmCpus, VmExitInfo, VM, phys_to_virt, VmCpuStatus
    },
    info,
};

use page_table_entry::MappingFlags;

#[cfg(target_arch = "riscv64")]
mod dtb_riscv64;
#[cfg(target_arch = "aarch64")]
mod dtb_aarch64;
#[cfg(target_arch = "aarch64")]
mod aarch64_config;

#[cfg(target_arch = "x86_64")]
mod x64;


// ADDED
use core::{sync::atomic::{AtomicUsize, Ordering}};

use lazy_init::LazyInit;
static mut HV_VM: LazyInit<VM<HyperCraftHalImpl, GuestPageTable>> = LazyInit::new();

static SYNC_VCPUS: AtomicUsize = AtomicUsize::new(0);

// TODO
static SMP: usize = 2;
// static SMP: usize = axconfig::SMP;


#[no_mangle]
fn main(hart_id: usize) {
    println!("Hello, hv!");
    
    // TODO
    assert!(hart_id == 0);

    #[cfg(target_arch = "riscv64")]
    {
        // boot cpu
        // PerCpu::<HyperCraftHalImpl>::init(0, 0x4000).unwrap();
        PerCpu::<HyperCraftHalImpl>::init(hart_id, 0x4000).unwrap();
        

        // get current percpu
        let pcpu = PerCpu::<HyperCraftHalImpl>::this_cpu();

        // create vcpu
        let gpt = setup_gpm(0x9000_0000).unwrap();
        // let mut vcpu = pcpu.create_vcpu(0, 0x9020_0000).unwrap();
        let mut vcpu = pcpu.create_vcpu(hart_id, 0x9020_0000).unwrap();
        vcpu.set_status(VmCpuStatus::Runnable);
        assert!(matches!(vcpu.get_status(), VmCpuStatus::Runnable));

        // multi-bounding
        let vcpu2 = pcpu.create_vcpu(hart_id + 1, 0x0).unwrap();
        assert!(matches!(vcpu2.get_status(), VmCpuStatus::PoweredOff));

        let mut vcpus = VmCpus::new();

        vcpus.add_vcpu(vcpu).unwrap();
        // multi-bounding
        vcpus.add_vcpu(vcpu2).unwrap();

        // let mut vm: VM<HyperCraftHalImpl, GuestPageTable> = VM::new(vcpus, gpt).unwrap();

        unsafe {
            HV_VM.init_by(VM::new(vcpus, gpt).unwrap());
        }
        let vm = unsafe { HV_VM.get_mut_unchecked() };

        // 等待同步
        info!("VCPU{} main hart ok and wait for sync", hart_id);

        SYNC_VCPUS.fetch_add(1, Ordering::Relaxed);
        SYNC_VCPUS.fetch_add(1, Ordering::Relaxed);
        while !(SYNC_VCPUS.load(Ordering::Acquire) == SMP) {
            core::hint::spin_loop();
        }

        info!("VCPU{} main hart sync done!!!", hart_id);

        let pcpu = PerCpu::<HyperCraftHalImpl>::this_cpu();
        loop {
            let next_vcpu_id = pcpu.next_vcpu();
            info!("HART{} run its vcpu{}", hart_id, next_vcpu_id);
            vm.init_vcpu(next_vcpu_id);
            vm.run(next_vcpu_id);
        }

        // vm.sync_vcpu(hart_id);
        vm.init_vcpu(hart_id);
        // vm run
        info!("vm run cpu{}", hart_id);
        // vm.run(0);
        vm.run(hart_id);
    }
    #[cfg(target_arch = "aarch64")]
    {
        // boot cpu
        PerCpu::<HyperCraftHalImpl>::init(0, 0x4000);   // change to pub const CPU_STACK_SIZE: usize = PAGE_SIZE * 128?

        // get current percpu
        let pcpu = PerCpu::<HyperCraftHalImpl>::this_cpu();

        // create vcpu, need to change addr for aarch64!
        let gpt = setup_gpm(0x7000_0000, 0x7020_0000).unwrap();  
        let vcpu = pcpu.create_vcpu(0).unwrap();
        let mut vcpus = VmCpus::new();

        // add vcpu into vm
        vcpus.add_vcpu(vcpu).unwrap();
        let mut vm: VM<HyperCraftHalImpl, GuestPageTable> = VM::new(vcpus, gpt, 0).unwrap();
        vm.init_vm_vcpu(0, 0x7020_0000, 0x7000_0000);

        info!("vm run cpu{}", hart_id);
        // suppose hart_id to be 0
        vm.run(0);
    }
    #[cfg(target_arch = "x86_64")]
    {
        println!("into main {}", hart_id);

        let mut p = PerCpu::<HyperCraftHalImpl>::new(hart_id);
        p.hardware_enable().unwrap();

        let gpm = x64::setup_gpm().unwrap();
        info!("{:#x?}", gpm);

        let mut vcpu = p
            .create_vcpu(x64::BIOS_ENTRY, gpm.nest_page_table_root())
            .unwrap();

        println!("Running guest...");
        vcpu.run();

        p.hardware_disable().unwrap();

        return;
    }
    #[cfg(not(any(target_arch = "riscv64", target_arch = "x86_64", target_arch = "aarch64")))]
    {
        panic!("Other arch is not supported yet!")
    }
}

#[cfg(target_arch = "riscv64")]
pub fn setup_gpm(dtb: usize) -> Result<GuestPageTable> {
    let mut gpt = GuestPageTable::new()?;
    let meta = MachineMeta::parse(dtb);
    if let Some(test) = meta.test_finisher_address {
        gpt.map_region(
            test.base_address,
            test.base_address,
            test.size + 0x1000,
            MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER | MappingFlags::EXECUTE,
        )?;
    }
    for virtio in meta.virtio.iter() {
        gpt.map_region(
            virtio.base_address,
            virtio.base_address,
            virtio.size,
            MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
        )?;
    }

    if let Some(uart) = meta.uart {
        error!("UART SIZE: {:#x}", uart.size);
        gpt.map_region(
            uart.base_address,
            uart.base_address,
            0x1000,
            // TODO NotAligned?
            // uart.size = 0x100,
            MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
        )?;
    }

    if let Some(clint) = meta.clint {
        gpt.map_region(
            clint.base_address,
            clint.base_address,
            clint.size,
            MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
        )?;
    }

    if let Some(plic) = meta.plic {
        error!("PLIC SIZE: {:#x}", plic.size);
        gpt.map_region(
            plic.base_address,
            plic.base_address,
            // TODO ang?为什么dtb里是600000?
            0x20_0000,
            // 0x60_0000,
            // 0x40_0000,
            // plic.size, 
            MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
        )?;
    }

    if let Some(pci) = meta.pci {
        gpt.map_region(
            pci.base_address,
            pci.base_address,
            pci.size,
            MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
        )?;
    }

    info!(
        "physical memory: [{:#x}: {:#x})",
        meta.physical_memory_offset,
        meta.physical_memory_offset + meta.physical_memory_size
    );

    gpt.map_region(
        meta.physical_memory_offset,
        meta.physical_memory_offset,
        meta.physical_memory_size,
        MappingFlags::READ | MappingFlags::WRITE | MappingFlags::EXECUTE | MappingFlags::USER,
    )?;

    Ok(gpt)
}

#[cfg(target_arch = "aarch64")]
pub fn setup_gpm(dtb: usize, kernel_entry: usize) -> Result<GuestPageTable> {
    let mut gpt = GuestPageTable::new()?;
    let meta = MachineMeta::parse(dtb);
    /* 
    for virtio in meta.virtio.iter() {
        gpt.map_region(
            virtio.base_address,
            virtio.base_address,
            0x1000, 
            MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
        )?;
        debug!("finish one virtio");
    }
    */
    // hard code for virtio_mmio
    gpt.map_region(
        0xa000000,
        0xa000000,
        0x4000,
        MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
    )?;
    
    if let Some(pl011) = meta.pl011 {
        gpt.map_region(
            pl011.base_address,
            pl011.base_address,
            pl011.size,
            MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
        )?;
    }

    if let Some(pl031) = meta.pl031 {
        gpt.map_region(
            pl031.base_address,
            pl031.base_address,
            pl031.size,
            MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
        )?;
    }

    if let Some(pl061) = meta.pl061 {
        gpt.map_region(
            pl061.base_address,
            pl061.base_address,
            pl061.size,
            MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
        )?;
    }

    for intc in meta.intc.iter() {
        gpt.map_region(
            intc.base_address,
            intc.base_address,
            intc.size,
            MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
        )?;
    }

    if let Some(pcie) = meta.pcie {
        gpt.map_region(
            pcie.base_address,
            pcie.base_address,
            pcie.size,
            MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
        )?;
    }

    for flash in meta.flash.iter() {
        gpt.map_region(
            flash.base_address,
            flash.base_address,
            flash.size,
            MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
        )?;
    }

    info!(
        "physical memory: [{:#x}: {:#x})",
        meta.physical_memory_offset,
        meta.physical_memory_offset + meta.physical_memory_size
    );
    
    gpt.map_region(
        meta.physical_memory_offset,
        meta.physical_memory_offset,
        meta.physical_memory_size,
        MappingFlags::READ | MappingFlags::WRITE | MappingFlags::EXECUTE | MappingFlags::USER,
    )?;
    
    gpt.map_region(
        GUEST_KERNEL_BASE_VADDR,
        kernel_entry,
        meta.physical_memory_size,
        MappingFlags::READ | MappingFlags::WRITE | MappingFlags::EXECUTE | MappingFlags::USER,
    )?;

    let gaddr:usize = 0x40_1000_0000;
    let paddr = gpt.translate(gaddr).unwrap();
    debug!("this is paddr for 0x{:X}: 0x{:X}", gaddr, paddr);
    Ok(gpt)
}


#[no_mangle]
pub extern "C" fn hv_secondary_main(hart_id: usize) {
    debug!("HART{} entering hv_secondary_main", hart_id);

    while let None = unsafe { HV_VM.try_get() } {
        core::hint::spin_loop();
    }

    PerCpu::<HyperCraftHalImpl>::setup_this_cpu(hart_id).unwrap();
    let pcpu = PerCpu::<HyperCraftHalImpl>::this_cpu();

    let vcpu = pcpu.create_vcpu(hart_id, 0).unwrap();

    // ADDED
    let vcpu2 = pcpu.create_vcpu(hart_id + 1, 0).unwrap();
    // let vcpu3 = pcpu.create_vcpu(hart_id + 2, 0).unwrap();

    assert!(matches!(vcpu.get_status(), VmCpuStatus::PoweredOff));
    info!("HART{} bounding VCPU{}", hart_id, vcpu.vcpu_id());

    let vm = unsafe { HV_VM.get_mut_unchecked() };

    vm.add_vcpu(vcpu).unwrap();
    // ADDED
    vm.add_vcpu(vcpu2).unwrap();
    // vm.add_vcpu(vcpu3).unwrap();
    
    
    info!("VCPU{} init ok and wait for sync", hart_id);

    SYNC_VCPUS.fetch_add(2, Ordering::Relaxed);
    // SYNC_VCPUS.fetch_add(1, Ordering::Relaxed);
    // SYNC_VCPUS.fetch_add(1, Ordering::Relaxed);
    // while !(SYNC_VCPUS.load(Ordering::Acquire) == SMP) {
    //     core::hint::spin_loop();
    // }

    // info!("VCPU{} sync done!!!", hart_id);

    let pcpu = PerCpu::<HyperCraftHalImpl>::this_cpu();
    loop {
        let next_vcpu_id = pcpu.next_vcpu();
        // info!("HART{} run its vcpu{}", hart_id, next_vcpu_id);
        vm.init_vcpu(next_vcpu_id);
        vm.run(next_vcpu_id);
    }

    // vm.sync_vcpu(hart_id);
    vm.init_vcpu(hart_id);
    vm.run(hart_id);
}