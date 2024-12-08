use std::fs::File;

fn main() {
    let mut naming = naming::NamespaceHandle::new();

    let name = "asfhbnwifnweifneijgnvoadenfjewnjofsndjoivgenivdmnsjonjisrnoijsnjibsdnsnjfokngjsofnjsoengjosengjoesnjogesnjogkendojgnjeonjosdvokdvnjokswmngorekgnvjoerngojewsngfoeswngfoesngokeswfmnoeswmfokeswfmkoeswfmnokesnvgokdernvgjoeswmngoeswmngoermngokrenfokeswnfokewngofeswngoewmnogewsn";

    naming.put(name, 5000.into());
    println!("val: {:?}", naming.get(name).unwrap());
}
