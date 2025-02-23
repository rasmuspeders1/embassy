#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]

use defmt::{info, unwrap, warn};
use embassy_executor::Spawner;
use embassy_net::tcp::TcpSocket;
use embassy_net::{Stack, StackResources};
use embassy_nrf::gpio::{AnyPin, Input, Level, Output, OutputDrive, Pin, Pull};
use embassy_nrf::rng::Rng;
use embassy_nrf::spim::{self, Spim};
use embassy_nrf::{bind_interrupts, peripherals};
use embassy_time::Delay;
use embedded_hal_async::spi::ExclusiveDevice;
use embedded_io::asynch::Write;
use static_cell::make_static;
use {defmt_rtt as _, embassy_net_esp_hosted as hosted, panic_probe as _};

bind_interrupts!(struct Irqs {
    SPIM3 => spim::InterruptHandler<peripherals::SPI3>;
    RNG => embassy_nrf::rng::InterruptHandler<peripherals::RNG>;
});

#[embassy_executor::task]
async fn wifi_task(
    runner: hosted::Runner<
        'static,
        ExclusiveDevice<Spim<'static, peripherals::SPI3>, Output<'static, peripherals::P0_31>, Delay>,
        Input<'static, AnyPin>,
        Output<'static, peripherals::P1_05>,
    >,
) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn net_task(stack: &'static Stack<hosted::NetDriver<'static>>) -> ! {
    stack.run().await
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    info!("Hello World!");

    let p = embassy_nrf::init(Default::default());

    let miso = p.P0_28;
    let sck = p.P0_29;
    let mosi = p.P0_30;
    let cs = Output::new(p.P0_31, Level::High, OutputDrive::HighDrive);
    let handshake = Input::new(p.P1_01.degrade(), Pull::Up);
    let ready = Input::new(p.P1_04.degrade(), Pull::None);
    let reset = Output::new(p.P1_05, Level::Low, OutputDrive::Standard);

    let mut config = spim::Config::default();
    config.frequency = spim::Frequency::M32;
    config.mode = spim::MODE_2; // !!!
    let spi = spim::Spim::new(p.SPI3, Irqs, sck, miso, mosi, config);
    let spi = ExclusiveDevice::new(spi, cs, Delay);

    let (device, mut control, runner) = embassy_net_esp_hosted::new(
        make_static!(embassy_net_esp_hosted::State::new()),
        spi,
        handshake,
        ready,
        reset,
    )
    .await;

    unwrap!(spawner.spawn(wifi_task(runner)));

    control.init().await;
    control.join(env!("WIFI_NETWORK"), env!("WIFI_PASSWORD")).await;

    let config = embassy_net::Config::dhcpv4(Default::default());
    // let config = embassy_net::Config::ipv4_static(embassy_net::StaticConfigV4 {
    //    address: Ipv4Cidr::new(Ipv4Address::new(10, 42, 0, 61), 24),
    //    dns_servers: Vec::new(),
    //    gateway: Some(Ipv4Address::new(10, 42, 0, 1)),
    // });

    // Generate random seed
    let mut rng = Rng::new(p.RNG, Irqs);
    let mut seed = [0; 8];
    rng.blocking_fill_bytes(&mut seed);
    let seed = u64::from_le_bytes(seed);

    // Init network stack
    let stack = &*make_static!(Stack::new(
        device,
        config,
        make_static!(StackResources::<2>::new()),
        seed
    ));

    unwrap!(spawner.spawn(net_task(stack)));

    // And now we can use it!

    let mut rx_buffer = [0; 4096];
    let mut tx_buffer = [0; 4096];
    let mut buf = [0; 4096];

    loop {
        let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);
        socket.set_timeout(Some(embassy_time::Duration::from_secs(10)));

        info!("Listening on TCP:1234...");
        if let Err(e) = socket.accept(1234).await {
            warn!("accept error: {:?}", e);
            continue;
        }

        info!("Received connection from {:?}", socket.remote_endpoint());

        loop {
            let n = match socket.read(&mut buf).await {
                Ok(0) => {
                    warn!("read EOF");
                    break;
                }
                Ok(n) => n,
                Err(e) => {
                    warn!("read error: {:?}", e);
                    break;
                }
            };

            info!("rxd {:02x}", &buf[..n]);

            match socket.write_all(&buf[..n]).await {
                Ok(()) => {}
                Err(e) => {
                    warn!("write error: {:?}", e);
                    break;
                }
            };
        }
    }
}
