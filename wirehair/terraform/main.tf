terraform {
  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 4.0"
    }
  }
}

provider "aws" {
  region = "ap-east-1"
}

resource "aws_vpc" "esys-wirehair" {
  cidr_block           = "10.0.0.0/16"
  enable_dns_hostnames = true
}

resource "aws_subnet" "esys-wirehair" {
  vpc_id                  = resource.aws_vpc.esys-wirehair.id
  cidr_block              = "10.0.0.0/16"
  map_public_ip_on_launch = true
}

resource "aws_internet_gateway" "esys-wirehair" {
  vpc_id = resource.aws_vpc.esys-wirehair.id
}

resource "aws_route_table" "esys-wirehair" {
  vpc_id = resource.aws_vpc.esys-wirehair.id

  route {
    cidr_block = "0.0.0.0/0"
    gateway_id = resource.aws_internet_gateway.esys-wirehair.id
  }
}

resource "aws_route_table_association" "_1" {
  route_table_id = resource.aws_route_table.esys-wirehair.id
  subnet_id      = resource.aws_subnet.esys-wirehair.id
}

resource "aws_security_group" "esys-wirehair" {
  vpc_id = resource.aws_vpc.esys-wirehair.id

  ingress {
    from_port        = 0
    to_port          = 0
    protocol         = "-1"
    cidr_blocks      = ["0.0.0.0/0"]
    ipv6_cidr_blocks = ["::/0"]
  }

  egress {
    from_port        = 0
    to_port          = 0
    protocol         = "-1"
    cidr_blocks      = ["0.0.0.0/0"]
    ipv6_cidr_blocks = ["::/0"]
  }
}

data "aws_ami" "ubuntu" {
  most_recent = true

  filter {
    name   = "name"
    values = ["ubuntu/images/hvm-ssd/ubuntu-jammy-22.04-amd64-server-*"]
  }

  filter {
    name   = "virtualization-type"
    values = ["hvm"]
  }

  owners = ["099720109477"] # Canonical
}

variable "instance_type" {
  type    = string
  default = "r5.xlarge"
}

resource "aws_instance" "esys-wirehair" {
  count = 10

  ami                    = data.aws_ami.ubuntu.id
  instance_type          = var.instance_type
  subnet_id              = resource.aws_subnet.esys-wirehair.id
  vpc_security_group_ids = [resource.aws_security_group.esys-wirehair.id]
  key_name               = "Ephemeral"
}

resource "local_file" "inventory" {
  content = templatefile(
    "${path.module}/inventory.ini.tftpl", {
      host = resource.aws_instance.esys-wirehair[*].public_dns
  })
  filename = "${path.module}/inventory.ini"
}
